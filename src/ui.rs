// ============================================================
//  ui.rs — SysCall Messenger TUI
//
//  Layout (fixed pane layout, utility-first design):
//
//  ╔═ SysCall Messenger ══════════════════════════════════════╗
//  ║  STATUS BAR: mode, pid, stmts, focus, compiler          ║
//  ╠══════════════════════╦═══════════════════════════════════╣
//  ║  CODE SPACE          ║  OUTPUT / RESULTS                ║
//  ║  (live C source)     ║  (scrollable kernel output log)  ║
//  ║                      ║                                  ║
//  ╠══════════════════════╩═══════════════════════════════════╣
//  ║  scm> input line                          [keybindings] ║
//  ╚═════════════════════════════════════════════════════════╝
//
//  Key Bindings:
//    Enter        → execute current input
//    Up/Down      → scroll output panel
//    PgUp/PgDn    → scroll code space panel
//    Ctrl+C/Esc   → quit
//    F1           → toggle help overlay
//    Ctrl+L       → clear output panel
//    Tab          → insert '    ' in input (indent)
// ============================================================

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};

use crate::compiler::{self, SessionRunner};

// ── Palette: plain, high-contrast, purpose-named ──────────────
const BG:          Color = Color::Black;
const PANEL_BG:    Color = Color::Rgb { r: 15, g: 15, b: 15 };
const BORDER:      Color = Color::Rgb { r: 100, g: 100, b: 100 };
const BORDER_ACT:  Color = Color::Rgb { r: 180, g: 180, b: 180 };
const TITLE_FG:    Color = Color::White;
const STATUS_FG:   Color = Color::Rgb { r: 200, g: 200, b: 200 };
const DIM_FG:      Color = Color::Rgb { r: 90, g: 90, b: 90 };

const KW_FG:       Color = Color::Rgb { r: 100, g: 160, b: 255 };  // C keywords / includes
const SYS_FG:      Color = Color::Rgb { r: 100, g: 220, b: 160 };  // syscall result / ok
const ERR_FG:      Color = Color::Rgb { r: 255, g: 100, b: 100 };  // error
const WARN_FG:     Color = Color::Rgb { r: 230, g: 200, b: 80 };   // warning / dim notice
const PROMPT_FG:   Color = Color::Rgb { r: 100, g: 220, b: 160 };  // scm>
const INPUT_FG:    Color = Color::White;
const LINE_NUM_FG: Color = Color::Rgb { r: 80, g: 80, b: 80 };
const FOCUS_FG:    Color = Color::Rgb { r: 255, g: 200, b: 60 };   // focused line marker
const OMIT_FG:     Color = Color::Rgb { r: 70, g: 70, b: 70 };     // omitted lines

/// An entry in the output log
#[derive(Clone)]
pub struct OutputLine {
    pub text: String,
    pub kind: LineKind,
}

#[derive(Clone, PartialEq)]
pub enum LineKind {
    Normal,
    KernelResult,
    Error,
    Warning,
    Cmd,
    Separator,
}

impl OutputLine {
    fn plain(s: impl Into<String>) -> Self { Self { text: s.into(), kind: LineKind::Normal } }
    fn cmd(s: impl Into<String>)   -> Self { Self { text: s.into(), kind: LineKind::Cmd } }
    fn sep()                        -> Self { Self { text: "─".repeat(60), kind: LineKind::Separator } }
    fn classify(s: &str) -> LineKind {
        let t = s.trim();
        if t.contains("[kernel return value:") || t.starts_with("[kernel ret") { return LineKind::KernelResult; }
        if t.starts_with('✗') || t.starts_with("error") || t.starts_with("Error") { return LineKind::Error; }
        if t.starts_with('⚑') || t.starts_with("warn") || t.starts_with("man lookup") { return LineKind::Warning; }
        LineKind::Normal
    }
    fn from_raw(s: impl Into<String>) -> Self {
        let text: String = s.into();
        let kind = Self::classify(&text);
        Self { text, kind }
    }
}

/// State shared between the main TUI loop and the command thread
pub struct TuiState {
    pub session: SessionRunner,
    pub output: Vec<OutputLine>,
    pub status: String,
    pub running: bool,       // command in-progress?
    pub quick_mode: bool,
    pub auto_confirm: bool,
    pub mem_inspect: bool,
    pub runner_session: bool,
    pub compiler_path: Option<String>,
}

impl TuiState {
    pub fn new(runner_session: bool, quick_mode: bool, auto_confirm: bool, mem_inspect: bool, compiler_path: Option<String>) -> Self {
        Self {
            session: SessionRunner::new(),
            output: Vec::new(),
            status: "Ready — type a syscall or command, press Enter to run".to_string(),
            running: false,
            quick_mode,
            auto_confirm,
            mem_inspect,
            runner_session,
            compiler_path,
        }
    }

    pub fn push_output(&mut self, lines: Vec<OutputLine>) {
        self.output.extend(lines);
        // Limit buffer to last 2000 lines
        if self.output.len() > 2000 {
            let drain_count = self.output.len() - 2000;
            self.output.drain(0..drain_count);
        }
    }
}

pub type SharedTui = Arc<Mutex<TuiState>>;

// ── Main TUI struct ──────────────────────────────────────────
pub struct Tui {
    state: SharedTui,
    input: String,
    input_cursor: usize,         // byte position of cursor in input
    out_scroll: usize,           // lines from bottom (0 = newest visible)
    cs_scroll: usize,            // code space panel scroll from top
    show_help: bool,
    history: Vec<String>,
    hist_idx: Option<usize>,
    last_input_save: String,     // saved input when browsing history
}

impl Tui {
    pub fn new(state: SharedTui) -> Self {
        Self {
            state,
            input: String::new(),
            input_cursor: 0,
            out_scroll: 0,
            cs_scroll: 0,
            show_help: false,
            history: Vec::new(),
            hist_idx: None,
            last_input_save: String::new(),
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide,
            crossterm::event::EnableMouseCapture)?;

        let result = self.event_loop(&mut stdout);

        execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show,
            crossterm::event::DisableMouseCapture)?;
        terminal::disable_raw_mode()?;
        result
    }

    fn event_loop(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        loop {
            let (cols, rows) = terminal::size()?;
            self.draw(stdout, cols, rows)?;
            stdout.flush()?;

            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key(key) {
                            break;
                        }
                    }
                    Event::Mouse(me) => {
                        self.handle_mouse(me, cols, rows);
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            // Check if background command finished (running flag cleared)
            // Re-draw handled by loop
        }
        Ok(())
    }

    // ── Key handling ────────────────────────────────────────────
    /// Returns true if should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Global quit
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        match key.code {
            // Toggle help
            KeyCode::F(1) => {
                self.show_help = !self.show_help;
            }

            // Clear output panel
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut s = self.state.lock().unwrap();
                s.output.clear();
                s.output.push(OutputLine::plain("  [output cleared]"));
            }

            // History up
            KeyCode::Up => {
                if self.history.is_empty() { return false; }
                match self.hist_idx {
                    None => {
                        self.last_input_save = self.input.clone();
                        self.hist_idx = Some(self.history.len() - 1);
                    }
                    Some(0) => {}
                    Some(i) => {
                        self.hist_idx = Some(i - 1);
                    }
                }
                if let Some(i) = self.hist_idx {
                    self.input = self.history[i].clone();
                    self.input_cursor = self.input.len();
                }
            }

            // History down
            KeyCode::Down => {
                match self.hist_idx {
                    None => {}
                    Some(i) if i + 1 >= self.history.len() => {
                        self.hist_idx = None;
                        self.input = self.last_input_save.clone();
                        self.input_cursor = self.input.len();
                    }
                    Some(i) => {
                        self.hist_idx = Some(i + 1);
                        self.input = self.history[i + 1].clone();
                        self.input_cursor = self.input.len();
                    }
                }
            }

            // Scroll output
            KeyCode::PageDown => {
                self.out_scroll = self.out_scroll.saturating_sub(5);
            }
            KeyCode::PageUp => {
                self.out_scroll = self.out_scroll.saturating_add(5);
            }

            // Cursor movement in input
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    // step back over multibyte chars
                    while !self.input.is_char_boundary(self.input_cursor) {
                        self.input_cursor -= 1;
                    }
                }
            }
            KeyCode::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor += 1;
                    while !self.input.is_char_boundary(self.input_cursor) {
                        self.input_cursor += 1;
                    }
                }
            }
            KeyCode::Home => { self.input_cursor = 0; }
            KeyCode::End  => { self.input_cursor = self.input.len(); }

            // Backspace
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    let mut del = self.input_cursor - 1;
                    while !self.input.is_char_boundary(del) { del -= 1; }
                    self.input.drain(del..self.input_cursor);
                    self.input_cursor = del;
                }
            }

            // Delete
            KeyCode::Delete => {
                if self.input_cursor < self.input.len() {
                    let mut end = self.input_cursor + 1;
                    while !self.input.is_char_boundary(end) { end += 1; }
                    self.input.drain(self.input_cursor..end);
                }
            }

            // Tab — insert 4 spaces
            KeyCode::Tab => {
                let spaces = "    ";
                self.input.insert_str(self.input_cursor, spaces);
                self.input_cursor += spaces.len();
            }

            // Enter — execute
            KeyCode::Enter => {
                let cmd = self.input.trim().to_string();
                if cmd.is_empty() { return false; }

                // Handle exit here
                if matches!(cmd.as_str(), "exit" | "quit" | "q") {
                    return true;
                }

                // Add to history
                if self.history.last().map(|l| l.as_str()) != Some(&cmd) {
                    self.history.push(cmd.clone());
                }
                self.hist_idx = None;

                self.input.clear();
                self.input_cursor = 0;
                self.out_scroll = 0; // snap to bottom

                self.execute_command(cmd);
            }

            // Character input
            KeyCode::Char(c) => {
                // Ignore modifiers other than shift
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    return false;
                }
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
            }

            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, me: MouseEvent, _cols: u16, _rows: u16) {
        match me.kind {
            MouseEventKind::ScrollUp => {
                self.out_scroll = self.out_scroll.saturating_add(3);
            }
            MouseEventKind::ScrollDown => {
                self.out_scroll = self.out_scroll.saturating_sub(3);
            }
            _ => {}
        }
    }

    // ── Command execution ───────────────────────────────────────
    fn execute_command(&mut self, cmd: String) {
        // Add command echo to output
        {
            let mut s = self.state.lock().unwrap();
            s.output.push(OutputLine::cmd(format!("scm> {}", cmd)));
            s.running = true;
            s.status = format!("Running: {}", &cmd[..cmd.len().min(60)]);
        }

        // Handle clr specially — resets session directly
        let cmd_lower = cmd.trim().to_lowercase();
        if matches!(cmd_lower.as_str(), "clr" | "clear" | "reset") {
            let mut s = self.state.lock().unwrap();
            s.session.reset();
            s.output.push(OutputLine::plain("  code space cleared."));
            s.output.push(OutputLine::sep());
            s.running = false;
            s.status = "Ready".to_string();
            return;
        }

        // Capture stdout/stderr from process_command via pipe
        let state_clone = Arc::clone(&self.state);
        let cmd_owned = cmd.clone();

        std::thread::spawn(move || {
            // Extract all the fields we need from state BEFORE releasing the lock
            // (We need to clone session so we can mutate it in another thread,
            //  then write it back.)
            let (mut session_snapshot, quick_mode, _auto_confirm, mem_inspect, runner_session, compiler_path) = {
                let s = state_clone.lock().unwrap();
                (
                    s.session.clone(),
                    s.quick_mode,
                    s.auto_confirm,
                    s.mem_inspect,
                    s.runner_session,
                    s.compiler_path.clone(),
                )
            };

            // Redirect stdout to a pipe so we capture compiler output
            let captured = capture_stdout(|| {
                crate::process_command(
                    &cmd_owned,
                    &mut session_snapshot,
                    quick_mode,
                    false, // editor mode off in TUI (no interactive prompts)
                    true,  // always auto-confirm in TUI
                    mem_inspect,
                    compiler_path.as_deref(),
                    runner_session,
                );
            });

            // Write session back and update output
            let mut s = state_clone.lock().unwrap();
            // Preserve daemon handle — session_snapshot.daemon was None after clone,
            // so we just update the non-daemon parts:
            s.session.extra_headers = session_snapshot.extra_headers;
            s.session.extra_vars    = session_snapshot.extra_vars;
            s.session.statements    = session_snapshot.statements;
            s.session.focus_index   = session_snapshot.focus_index;

            // Parse captured output into OutputLines
            let mut new_lines: Vec<OutputLine> = Vec::new();
            for line in captured.lines() {
                // Strip ANSI escape sequences for clean TUI rendering
                let clean = strip_ansi(line);
                new_lines.push(OutputLine::from_raw(clean));
            }
            if new_lines.is_empty() {
                new_lines.push(OutputLine::plain("  (no output)"));
            }
            new_lines.push(OutputLine::sep());
            s.push_output(new_lines);
            s.running = false;
            s.status = "Ready".to_string();
        });
    }

    // ── Drawing ─────────────────────────────────────────────────
    fn draw(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        let s = self.state.lock().unwrap();

        // Full clear
        queue!(stdout, cursor::MoveTo(0, 0), SetBackgroundColor(BG))?;
        for r in 0..rows {
            queue!(stdout, cursor::MoveTo(0, r), Print(" ".repeat(cols as usize)))?;
        }

        // Layout measurements
        let status_h: u16 = 1;
        let input_h: u16  = 1;
        let separator_h: u16 = 1;           // horizontal divider between panels
        let content_h = rows.saturating_sub(status_h + input_h + separator_h + 2); // +2 for top/bottom border rows
        // Panel horizontal split
        // If terminal is wide enough, side-by-side; otherwise stack (code space collapses)
        let min_side_by_side_cols: u16 = 100;
        let (cs_w, out_x, out_w): (u16, u16, u16) = if cols >= min_side_by_side_cols {
            let cs = (cols / 2).min(60);
            (cs, cs + 1, cols.saturating_sub(cs + 1))
        } else {
            // Narrow: show output only, code space via `cs` command
            (0, 0, cols)
        };

        let content_top: u16 = status_h + 1; // after status + top border

        // ── Status bar (row 0) ──────────────────────────────────
        self.draw_status(stdout, cols, 0, &s)?;

        // ── Top border (row 1) ──────────────────────────────────
        self.draw_top_border(stdout, cols, status_h, cs_w, out_x)?;

        // ── Code space panel ────────────────────────────────────
        if cs_w > 0 {
            self.draw_codespace_panel(stdout, 0, cs_w, content_top, content_h, &s)?;
            // Vertical divider
            self.draw_vdivider(stdout, cs_w, content_top, content_h)?;
        }

        // ── Output panel ────────────────────────────────────────
        self.draw_output_panel(stdout, out_x, out_w, content_top, content_h, &s)?;

        // ── Horizontal divider above input ──────────────────────
        let divider_row = content_top + content_h;
        self.draw_h_divider(stdout, cols, divider_row)?;

        // ── Input bar ───────────────────────────────────────────
        let input_row = divider_row + 1;
        self.draw_input(stdout, cols, input_row, s.running)?;

        // ── Bottom border ───────────────────────────────────────
        let bottom_row = input_row + 1;
        if bottom_row < rows {
            self.draw_bottom_border(stdout, cols, bottom_row, cs_w)?;
        }

        // ── Help overlay ────────────────────────────────────────
        if self.show_help {
            self.draw_help(stdout, cols, rows)?;
        }

        Ok(())
    }

    fn draw_status(&self, stdout: &mut io::Stdout, cols: u16, row: u16, s: &TuiState) -> io::Result<()> {
        let mode_tag = if s.runner_session { "[-R persistent]" } else { "[single-shot]" };
        let stmts_tag = format!("stmts:{}", s.session.statements.len());
        let focus_tag = match s.session.focus_index {
            Some(i) => format!("focus:[{}]", i + 1),
            None    => format!("focus:[last]"),
        };
        let run_tag = if s.running { " RUNNING " } else { "" };

        let left = format!(" SysCall Messenger TUI  {}  {}  {}",
            mode_tag, stmts_tag, focus_tag);
        let right = format!("{}F1=help  Ctrl+C=quit ", run_tag);
        let pad = (cols as usize).saturating_sub(left.len() + right.len());

        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(TITLE_FG),
            Print(&left),
            SetForegroundColor(if s.running { WARN_FG } else { DIM_FG }),
            Print(" ".repeat(pad)),
            SetForegroundColor(DIM_FG),
            Print(&right),
            ResetColor,
        )?;
        Ok(())
    }

    fn draw_top_border(&self, stdout: &mut io::Stdout, cols: u16, row: u16, cs_w: u16, out_x: u16) -> io::Result<()> {
        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(BORDER),
        )?;
        if cs_w > 0 {
            // ┌── CODE SPACE ──┬── OUTPUT ──┐
            let cs_title = " CODE SPACE ";
            let out_title = " OUTPUT ";
            let cs_fill = (cs_w as usize).saturating_sub(cs_title.len() + 2);
            let out_fill = (out_x as usize + cols as usize - out_x as usize).saturating_sub(out_title.len() + 3);
            queue!(stdout,
                Print("┌"),
                SetForegroundColor(BORDER_ACT),
                Print(cs_title),
                SetForegroundColor(BORDER),
                Print("─".repeat(cs_fill)),
                Print("┬"),
                SetForegroundColor(BORDER_ACT),
                Print(out_title),
                SetForegroundColor(BORDER),
                Print("─".repeat(out_fill.saturating_sub(1))),
                Print("┐"),
            )?;
        } else {
            let out_title = " OUTPUT ";
            let fill = (cols as usize).saturating_sub(out_title.len() + 2);
            queue!(stdout,
                Print("┌"),
                SetForegroundColor(BORDER_ACT),
                Print(out_title),
                SetForegroundColor(BORDER),
                Print("─".repeat(fill)),
                Print("┐"),
            )?;
        }
        Ok(())
    }

    fn draw_vdivider(&self, stdout: &mut io::Stdout, x: u16, top: u16, height: u16) -> io::Result<()> {
        for r in top..top+height {
            queue!(stdout,
                cursor::MoveTo(x, r),
                SetBackgroundColor(BG),
                SetForegroundColor(BORDER),
                Print("│"),
            )?;
        }
        Ok(())
    }

    fn draw_h_divider(&self, stdout: &mut io::Stdout, cols: u16, row: u16) -> io::Result<()> {
        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(BORDER),
            Print("├"),
            Print("─".repeat(cols as usize - 2)),
            Print("┤"),
        )?;
        Ok(())
    }

    fn draw_bottom_border(&self, stdout: &mut io::Stdout, cols: u16, row: u16, _cs_w: u16) -> io::Result<()> {
        let keybinds = " ↑↓ scroll  PgUp/PgDn code  Ctrl+L clear  F1 help ";
        let fill = (cols as usize).saturating_sub(keybinds.len() + 2);
        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(BORDER),
            Print("└"),
            SetForegroundColor(DIM_FG),
            Print("─".repeat(fill.min(cols as usize - 2))),
            SetForegroundColor(DIM_FG),
            Print(if cols as usize > keybinds.len() + 2 { keybinds } else { "" }),
            SetForegroundColor(BORDER),
            Print("┘"),
        )?;
        Ok(())
    }

    fn draw_input(&self, stdout: &mut io::Stdout, cols: u16, row: u16, running: bool) -> io::Result<()> {
        let prompt = "scm> ";
        let available = (cols as usize).saturating_sub(prompt.len() + 2 + 1); // 2 for borders, 1 for cursor

        // Scroll input view to keep cursor visible
        let cur = self.input_cursor;
        let disp_start = if cur > available { cur - available } else { 0 };
        let disp_input = &self.input[disp_start..];
        let disp_input = if disp_input.len() > available { &disp_input[..available] } else { disp_input };

        let cursor_visible_pos = cur - disp_start;
        let before_cur = &disp_input[..cursor_visible_pos.min(disp_input.len())];
        let after_cur  = if cursor_visible_pos < disp_input.len() { &disp_input[cursor_visible_pos..] } else { "" };

        let pad = available.saturating_sub(disp_input.len() + 1);

        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(BORDER),
            Print("│"),
            SetForegroundColor(if running { WARN_FG } else { PROMPT_FG }),
            Print(prompt),
            SetForegroundColor(INPUT_FG),
            Print(before_cur),
            // cursor block
            SetBackgroundColor(Color::White),
            SetForegroundColor(Color::Black),
            Print(if after_cur.is_empty() { " " } else { &after_cur[..after_cur.chars().next().map_or(0, |c| c.len_utf8())] }),
            SetBackgroundColor(BG),
            SetForegroundColor(INPUT_FG),
            Print(if after_cur.len() > after_cur.chars().next().map_or(0, |c| c.len_utf8()) {
                &after_cur[after_cur.chars().next().map_or(0, |c| c.len_utf8())..]
            } else { "" }),
            Print(" ".repeat(pad)),
            SetForegroundColor(BORDER),
            Print("│"),
        )?;
        Ok(())
    }

    fn draw_codespace_panel(
        &self, stdout: &mut io::Stdout,
        x: u16, width: u16, top: u16, height: u16,
        s: &TuiState,
    ) -> io::Result<()> {
        let inner_w = width.saturating_sub(1) as usize;

        let source_lines: Vec<String> = match compiler::get_session_source(&s.session) {
            Some(src) => src.lines().map(|l| l.to_string()).collect(),
            None => vec!["  (empty code space)".to_string(), "".to_string(),
                         "  Type a syscall to add statements.".to_string(),
                         "  e.g:  getpid()".to_string(),
                         "        openat(AT_FDCWD, \"file\", O_RDONLY)".to_string(),
                         "".to_string(),
                         "  Commands: cs, cs run, fc, rpc, ins, clr".to_string()],
        };

        // Get focus info for marking
        let focus_idx = compiler::get_statement_lines(&s.session);
        let focused_stmt_idx = s.session.focus_index;

        let total = source_lines.len();
        let start = self.cs_scroll.min(total.saturating_sub(height as usize));

        for (r_off, r) in (top..top+height).enumerate() {
            let line_idx = start + r_off;
            queue!(stdout,
                cursor::MoveTo(x, r),
                SetBackgroundColor(PANEL_BG),
            )?;

            if line_idx < total {
                let line = &source_lines[line_idx];
                let line_no = line_idx + 1;

                // Determine color by content
                let trimmed = line.trim();
                let is_focused_c_line = is_line_focused(line_no, &focus_idx, focused_stmt_idx);
                let is_omitted = is_line_omitted(line_no, &focus_idx, focused_stmt_idx, &s.session);

                let content_color = if is_omitted {
                    OMIT_FG
                } else if trimmed.starts_with("#include") {
                    KW_FG
                } else if trimmed.contains('(') && !trimmed.starts_with("int main")
                       && !trimmed.starts_with("if ") && !trimmed.starts_with("printf") {
                    SYS_FG
                } else {
                    STATUS_FG
                };

                let num_str = format!("{:3}", line_no);
                let focus_marker = if is_focused_c_line { "▶" } else { " " };

                // Render: linenum | marker | content (truncated)
                let prefix_len = num_str.len() + 1 + 2; // "NNN│ ▶"
                let max_content = inner_w.saturating_sub(prefix_len);
                let content = if line.len() > max_content { &line[..max_content] } else { line.as_str() };
                let pad = max_content.saturating_sub(content.len());

                queue!(stdout,
                    SetForegroundColor(LINE_NUM_FG),
                    Print(&num_str),
                    SetForegroundColor(BORDER),
                    Print("│"),
                    SetForegroundColor(if is_focused_c_line { FOCUS_FG } else { DIM_FG }),
                    Print(focus_marker),
                    SetForegroundColor(content_color),
                    Print(content),
                    Print(" ".repeat(pad)),
                )?;
            } else {
                queue!(stdout, Print(" ".repeat(inner_w)))?;
            }
        }
        Ok(())
    }

    fn draw_output_panel(
        &self, stdout: &mut io::Stdout,
        x: u16, width: u16, top: u16, height: u16,
        s: &TuiState,
    ) -> io::Result<()> {
        let inner_w = width.saturating_sub(2) as usize; // 1 for left border, 1 for right border
        let total = s.output.len();
        let h = height as usize;

        // Scroll from bottom
        let end = total.saturating_sub(self.out_scroll);
        let start = end.saturating_sub(h);

        for (r_off, r) in (top..top+height).enumerate() {
            let line_idx = start + r_off;
            queue!(stdout,
                cursor::MoveTo(x, r),
                SetBackgroundColor(BG),
                SetForegroundColor(BORDER),
                Print("│"),
                SetBackgroundColor(BG),
            )?;

            if line_idx < end && line_idx < total {
                let ol = &s.output[line_idx];
                let color = match ol.kind {
                    LineKind::KernelResult => SYS_FG,
                    LineKind::Error        => ERR_FG,
                    LineKind::Warning      => WARN_FG,
                    LineKind::Cmd          => PROMPT_FG,
                    LineKind::Separator    => BORDER,
                    LineKind::Normal       => STATUS_FG,
                };

                let text = &ol.text;
                let content = if text.len() > inner_w { &text[..inner_w] } else { text.as_str() };
                let pad = inner_w.saturating_sub(content.len());
                queue!(stdout,
                    SetForegroundColor(color),
                    Print(content),
                    Print(" ".repeat(pad)),
                )?;
            } else {
                queue!(stdout, Print(" ".repeat(inner_w)))?;
            }

            queue!(stdout,
                SetForegroundColor(BORDER),
                Print("│"),
            )?;
        }
        Ok(())
    }

    fn draw_help(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        let lines = [
            "──── KEYBINDINGS ──────────────────────────────",
            " Enter        Execute command / syscall",
            " Up / Down    Browse command history",
            " PgUp / PgDn  Scroll output panel",
            " ← / →        Move cursor in input",
            " Home / End   Jump to start / end of input",
            " Backspace    Delete character before cursor",
            " Delete       Delete character after cursor",
            " Tab          Insert 4 spaces",
            " Ctrl+L       Clear output panel",
            " Mouse Wheel  Scroll output panel",
            " F1           Toggle this help",
            " Ctrl+C / Esc Quit",
            "──── COMMANDS ─────────────────────────────────",
            " getpid()                Execute syscall",
            " cs                      View code space",
            " cs run                  Compile & run",
            " fc [L<n>|<n>]           Set focus point",
            " rpc L<n> <code>         Replace line",
            " rpc L<n> NULL           Delete line",
            " rpc L*                  Clear all lines",
            " ins L<n> <code>         Insert line",
            " ins L<n> NULL           Insert blank line",
            " session                 Show session state",
            " clr                     Reset code space",
            " exit / q                Quit TUI",
            "───────────────────────────────────────────────",
            " Press F1 to close this help",
        ];

        let box_w = 50usize;
        let box_h = lines.len() + 2;
        let start_col = ((cols as usize).saturating_sub(box_w)) / 2;
        let start_row = ((rows as usize).saturating_sub(box_h)) / 2;

        // Top border
        queue!(stdout,
            cursor::MoveTo(start_col as u16, start_row as u16),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(BORDER_ACT),
            Print("┌"),
            Print("─".repeat(box_w - 2)),
            Print("┐"),
        )?;

        for (i, line) in lines.iter().enumerate() {
            let row = start_row + 1 + i;
            let padded = format!("{:<width$}", line, width = box_w - 2);
            let fg = if line.starts_with("────") { BORDER } else { STATUS_FG };
            queue!(stdout,
                cursor::MoveTo(start_col as u16, row as u16),
                SetBackgroundColor(PANEL_BG),
                SetForegroundColor(BORDER_ACT),
                Print("│"),
                SetForegroundColor(fg),
                Print(&padded),
                SetForegroundColor(BORDER_ACT),
                Print("│"),
            )?;
        }

        // Bottom border
        queue!(stdout,
            cursor::MoveTo(start_col as u16, (start_row + box_h - 1) as u16),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(BORDER_ACT),
            Print("└"),
            Print("─".repeat(box_w - 2)),
            Print("┘"),
        )?;

        Ok(())
    }
}

// ── Helper: check if a C source line number corresponds to the focused statement
fn is_line_focused(
    line_no: usize,
    focus_info: &[(usize, usize, usize, String, bool)],
    _focused_stmt_idx: Option<usize>,
) -> bool {
    for (_, start, end, _, is_focused) in focus_info {
        if *is_focused && line_no >= *start && line_no <= *end {
            return true;
        }
    }
    false
}

fn is_line_omitted(
    line_no: usize,
    focus_info: &[(usize, usize, usize, String, bool)],
    _focused_stmt_idx: Option<usize>,
    session: &SessionRunner,
) -> bool {
    // Lines past the last focused C line are omitted
    if focus_info.is_empty() { return false; }
    if session.statements.is_empty() { return false; }

    // Find the last line of the focused statement
    let focused_end = focus_info.iter()
        .filter(|(_, _, _, _, is_f)| *is_f)
        .map(|(_, _, end, _, _)| *end)
        .max();

    if let Some(fe) = focused_end {
        // Lines past focused_end and before "return 0;" and "}" are omitted
        if line_no > fe {
            // But don't mark the closing braces / return as omitted
            return true;
        }
    }
    false
}

// ── Strip ANSI escape codes from a string ──────────────────────
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume until letter
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc.is_ascii_alphabetic() || nc == 'm' { break; }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Capture stdout from a closure via pipe ──────────────────────
pub fn capture_stdout<F: FnOnce()>(f: F) -> String {
    use std::os::unix::io::RawFd;

    unsafe {
        // Create pipe: pipefd[0]=read end, pipefd[1]=write end
        let mut pipefd: [RawFd; 2] = [0; 2];
        if libc::pipe(pipefd.as_mut_ptr()) != 0 {
            f();
            return String::new();
        }

        // Save original stdout fd
        let saved_stdout = libc::dup(1);
        let saved_stderr = libc::dup(2);

        // Redirect stdout and stderr to write end of pipe
        libc::dup2(pipefd[1], 1);
        libc::dup2(pipefd[1], 2);
        libc::close(pipefd[1]);

        // Run the closure
        f();

        // Flush C stdio buffers
        libc::fflush(std::ptr::null_mut());

        // Restore stdout and stderr
        libc::dup2(saved_stdout, 1);
        libc::dup2(saved_stderr, 2);
        libc::close(saved_stdout);
        libc::close(saved_stderr);

        // Read all output from pipe read end (non-blocking after close of write end)
        let mut output = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = libc::read(pipefd[0], buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            if n <= 0 { break; }
            output.extend_from_slice(&buf[..n as usize]);
        }
        libc::close(pipefd[0]);

        String::from_utf8_lossy(&output).into_owned()
    }
}

// ── Public TUI entry point ────────────────────────────────────
pub fn run_tui(
    runner_session: bool,
    quick_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<String>,
) {
    let state = Arc::new(Mutex::new(TuiState::new(
        runner_session, quick_mode, auto_confirm, mem_inspect, compiler_path,
    )));

    // Initial welcome message
    {
        let mut s = state.lock().unwrap();
        s.output.push(OutputLine::plain("  SysCall Messenger TUI — ready."));
        s.output.push(OutputLine::plain("  Type any syscall or libc function (e.g. getpid())"));
        if runner_session {
            s.output.push(OutputLine::plain("  Persistent session (-R) active — state persists across calls."));
        }
        s.output.push(OutputLine::plain("  Press F1 for help. Ctrl+C or 'exit' to quit."));
        s.output.push(OutputLine::sep());
    }

    let mut tui = Tui::new(Arc::clone(&state));
    if let Err(e) = tui.run() {
        eprintln!("TUI error: {}", e);
    }
}
