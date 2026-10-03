// ============================================================
//  ui.rs — SysCall Messenger TUI
//
//  Layout: single full-screen pane (like a terminal REPL).
//
//  ┌─────────────────────────────────────────────────────────┐
//  │  SysCall Messenger  [-R] [-q]  stmts:3  focus:[2]  F1=help │  ← header
//  ├─────────────────────────────────────────────────────────┤
//  │                                                         │
//  │   (scrollable output log — clickable line selection)    │  ← main area
//  │                                                         │
//  ├─────────────────────────────────────────────────────────┤
//  │  scm> _                                                 │  ← input bar
//  └─────────────────────────────────────────────────────────┘
//
//  Keys:
//    Enter        run command (or copy selected line if input empty)
//    Up/Down      history / navigate & select output lines
//    PgUp/PgDn   scroll output by page
//    Home/End     cursor to start/end of input
//    Left/Right   move cursor in input (or cycle compiler in flags)
//    Backspace    delete before cursor
//    Delete       delete after cursor
//    Ctrl+L       clear output
//    F1           show/hide help overlay
//    F2 or 'f'    show/hide flags & modes graphical modal
//    Esc          clear line selection / close overlay / quit
//    Ctrl+C       quit
// ============================================================

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};

use crate::compiler::SessionRunner;

// ── Minimal, readable colour palette ─────────────────────────
const BG:          Color = Color::Black;
const FG:          Color = Color::Rgb { r: 210, g: 210, b: 210 };
const HEADER_BG:   Color = Color::Rgb { r: 30,  g: 30,  b: 30  };
const HEADER_FG:   Color = Color::White;
const BORDER:      Color = Color::Rgb { r: 80,  g: 80,  b: 80  };
const PROMPT:      Color = Color::Rgb { r: 80,  g: 200, b: 120 };  // green scm>
const DIM:         Color = Color::Rgb { r: 80,  g: 80,  b: 80  };
const RESULT_OK:   Color = Color::Rgb { r: 80,  g: 200, b: 120 };  // kernel result ok
const RESULT_ERR:  Color = Color::Rgb { r: 220, g: 80,  b: 80  };  // error
const RESULT_WARN: Color = Color::Rgb { r: 210, g: 180, b: 60  };  // warning/notice
const CMD_ECHO:    Color = Color::Rgb { r: 100, g: 160, b: 255 };  // echoed command
const SEL_BG:      Color = Color::Rgb { r: 35,  g: 75,  b: 140 };  // selected line blue bg

// ── Output line kinds ─────────────────────────────────────────
#[derive(Clone, PartialEq)]
pub enum Kind {
    Normal,
    CmdEcho,
    KernelOk,
    KernelErr,
    Warning,
    Separator,
    Help,
}

#[derive(Clone)]
pub struct Line {
    pub text: String,
    pub kind: Kind,
}

impl Line {
    fn new(text: impl Into<String>, kind: Kind) -> Self { Self { text: text.into(), kind } }
    fn sep() -> Self { Self::new("", Kind::Separator) }

    fn classify(s: &str) -> Kind {
        let t = s.trim();
        if t.contains("[kernel return value:") { return Kind::KernelOk; }
        if t.starts_with('✗') || t.to_lowercase().starts_with("error") { return Kind::KernelErr; }
        if t.starts_with('✓') || t.starts_with("man lookup") || t.starts_with('⚑') { return Kind::Warning; }
        Kind::Normal
    }
    fn from_output(s: impl Into<String>) -> Self {
        let text: String = s.into();
        let kind = Self::classify(&text);
        Self { text, kind }
    }
}

// ── Shared TUI state (passed into execute thread) ─────────────
pub struct TuiState {
    pub session: SessionRunner,
    pub lines:   Vec<Line>,           // output log
    pub running: bool,
    pub status:  String,
    // session config
    pub runner_session: bool,
    pub quick_mode:     bool,
    pub auto_confirm:   bool,
    pub mem_inspect:    bool,
    pub compiler_path:  Option<String>,
}

impl TuiState {
    pub fn new(runner_session: bool, quick_mode: bool, auto_confirm: bool, mem_inspect: bool, compiler_path: Option<String>) -> Self {
        Self {
            session: SessionRunner::new(),
            lines: Vec::new(),
            running: false,
            status: String::new(),
            runner_session,
            quick_mode,
            auto_confirm,
            mem_inspect,
            compiler_path,
        }
    }

    fn push_lines(&mut self, new: Vec<Line>) {
        self.lines.extend(new);
        if self.lines.len() > 5000 {
            let drop = self.lines.len() - 5000;
            self.lines.drain(0..drop);
        }
    }
}

pub type Shared = Arc<Mutex<TuiState>>;

// ── Main TUI ─────────────────────────────────────────────────
pub struct Tui {
    shared:             Shared,
    input:              String,
    cursor:             usize,           // byte position
    scroll:             usize,           // lines hidden at bottom (0 = newest visible)
    history:            Vec<String>,
    hist_idx:           Option<usize>,
    hist_save:          String,
    show_help:          bool,
    show_flags:         bool,
    flags_selected_idx: usize,
    selected_line:      Option<usize>,   // currently selected output line index
}

impl Tui {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            input: String::new(),
            cursor: 0,
            scroll: 0,
            history: Vec::new(),
            hist_idx: None,
            hist_save: String::new(),
            show_help: false,
            show_flags: false,
            flags_selected_idx: 0,
            selected_line: None,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            crossterm::event::EnableMouseCapture,
        )?;

        let result = self.event_loop(&mut stdout);

        // Always restore terminal even on panic-exit
        let _ = execute!(stdout,
            terminal::LeaveAlternateScreen,
            cursor::Show,
            crossterm::event::DisableMouseCapture,
        );
        let _ = terminal::disable_raw_mode();
        result
    }

    // ── Event loop ──────────────────────────────────────────────
    fn event_loop(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        loop {
            let (cols, rows) = terminal::size()?;
            self.draw(stdout, cols, rows)?;
            stdout.flush()?;

            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    Event::Key(k) => { if self.on_key(k) { break; } }
                    Event::Mouse(m) => { self.on_mouse(m); }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    // ── Key handler ─────────────────────────────────────────────
    fn on_key(&mut self, key: KeyEvent) -> bool {
        // Esc key handling
        if key.code == KeyCode::Esc {
            if self.show_flags {
                self.show_flags = false;
                return false;
            }
            if self.show_help {
                self.show_help = false;
                return false;
            }
            if self.selected_line.is_some() {
                self.selected_line = None;
                return false;
            }
            return true; // Quit TUI
        }

        // Ctrl+C key handling
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }

        // F1: Help overlay
        if key.code == KeyCode::F(1) {
            self.show_help = !self.show_help;
            if self.show_help { self.show_flags = false; }
            return false;
        }

        // F2: Flags modal
        if key.code == KeyCode::F(2) {
            self.show_flags = !self.show_flags;
            if self.show_flags { self.show_help = false; }
            return false;
        }

        // Intercept keys when Flags Modal is open
        if self.show_flags {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.show_flags = false;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.flags_selected_idx > 0 {
                        self.flags_selected_idx -= 1;
                    } else {
                        self.flags_selected_idx = 4;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.flags_selected_idx < 4 {
                        self.flags_selected_idx += 1;
                    } else {
                        self.flags_selected_idx = 0;
                    }
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    self.toggle_flag_at(self.flags_selected_idx);
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.cycle_compiler();
                }
                KeyCode::Char('1') => self.toggle_flag_at(0),
                KeyCode::Char('2') => self.toggle_flag_at(1),
                KeyCode::Char('3') => self.toggle_flag_at(2),
                KeyCode::Char('4') => self.toggle_flag_at(3),
                KeyCode::Char('5') => self.cycle_compiler(),
                _ => {}
            }
            return false;
        }

        // Any key dismisses Help overlay
        if self.show_help {
            self.show_help = false;
            return false;
        }

        // Open Flags modal if typing 'f' or 'F' when input bar is empty and no line selected
        if (key.code == KeyCode::Char('f') || key.code == KeyCode::Char('F'))
            && self.input.is_empty()
            && self.selected_line.is_none()
            && key.modifiers.is_empty()
        {
            self.show_flags = true;
            return false;
        }

        // Copy selected line into input bar if user presses 'c' or 'y' when input is empty & line selected
        if (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('y'))
            && self.input.is_empty()
            && key.modifiers.is_empty()
            && self.selected_line.is_some()
        {
            self.copy_selected_to_input();
            return false;
        }

        // Clear output
        if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let mut s = self.shared.lock().unwrap();
            s.lines.clear();
            self.selected_line = None;
            return false;
        }

        match key.code {
            // Line selection & Scroll with Up/Down when input is empty
            KeyCode::Up => {
                if self.input.is_empty() && self.hist_idx.is_none() {
                    let s = self.shared.lock().unwrap();
                    let total = s.lines.len();
                    if total > 0 {
                        let cur = self.selected_line.unwrap_or(total);
                        if cur > 0 {
                            let new_sel = cur - 1;
                            self.selected_line = Some(new_sel);
                            // Adjust scroll so new_sel is visible
                            let (_cols, rows) = terminal::size().unwrap_or((80, 24));
                            let h = (rows as usize).saturating_sub(5);
                            let end = total.saturating_sub(self.scroll);
                            let start = end.saturating_sub(h);
                            if new_sel < start {
                                self.scroll = total.saturating_sub(new_sel);
                            } else if new_sel >= end {
                                self.scroll = total.saturating_sub(new_sel + 1);
                            }
                        }
                    }
                } else {
                    self.history_prev();
                }
            }
            KeyCode::Down => {
                if self.input.is_empty() && self.hist_idx.is_none() {
                    let s = self.shared.lock().unwrap();
                    let total = s.lines.len();
                    if total > 0 {
                        if let Some(cur) = self.selected_line {
                            if cur + 1 < total {
                                let new_sel = cur + 1;
                                self.selected_line = Some(new_sel);
                                // Adjust scroll so new_sel is visible
                                let (_cols, rows) = terminal::size().unwrap_or((80, 24));
                                let _h = (rows as usize).saturating_sub(5);
                                let end = total.saturating_sub(self.scroll);
                                if new_sel >= end {
                                    self.scroll = total.saturating_sub(new_sel + 1);
                                }
                            }
                        }
                    }
                } else {
                    self.history_next();
                }
            }

            KeyCode::PageUp   => {
                self.scroll = self.scroll.saturating_add(10);
                self.selected_line = None;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                self.selected_line = None;
            }

            KeyCode::Left  => { self.cursor_left(); }
            KeyCode::Right => { self.cursor_right(); }
            KeyCode::Home  => { self.cursor = 0; }
            KeyCode::End   => { self.cursor = self.input.len(); }

            KeyCode::Backspace => { self.backspace(); }
            KeyCode::Delete    => { self.delete_fwd(); }

            KeyCode::Tab => {
                self.input.insert_str(self.cursor, "    ");
                self.cursor += 4;
            }

            KeyCode::Enter => {
                // If input is empty but a line is selected, copy it to input
                if self.input.is_empty() && self.selected_line.is_some() {
                    self.copy_selected_to_input();
                    return false;
                }

                let cmd = self.input.trim().to_string();
                self.input.clear();
                self.cursor = 0;
                self.scroll = 0;        // snap to newest output
                self.hist_idx = None;
                self.selected_line = None;

                if cmd.is_empty() { return false; }
                if matches!(cmd.as_str(), "exit" | "quit" | "q") { return true; }

                // Add to history (deduplicate adjacent)
                if self.history.last().map(String::as_str) != Some(&cmd) {
                    self.history.push(cmd.clone());
                }

                self.execute(cmd);
            }

            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                if self.scroll > 0 { self.scroll = 0; }
                self.selected_line = None;
            }

            _ => {}
        }
        false
    }

    fn copy_selected_to_input(&mut self) {
        if let Some(sel) = self.selected_line {
            let s = self.shared.lock().unwrap();
            if sel < s.lines.len() {
                let text = s.lines[sel].text.clone();
                let clean_text = if text.starts_with("scm> ") {
                    text[5..].trim().to_string()
                } else {
                    text.trim().to_string()
                };
                if !clean_text.is_empty() {
                    self.input = clean_text;
                    self.cursor = self.input.len();
                    self.selected_line = None;
                }
            }
        }
    }

    fn toggle_flag_at(&mut self, idx: usize) {
        let mut s = self.shared.lock().unwrap();
        match idx {
            0 => s.runner_session = !s.runner_session,
            1 => s.quick_mode     = !s.quick_mode,
            2 => s.mem_inspect    = !s.mem_inspect,
            3 => s.auto_confirm  = !s.auto_confirm,
            4 => {
                drop(s);
                self.cycle_compiler();
            }
            _ => {}
        }
    }

    fn cycle_compiler(&mut self) {
        let mut s = self.shared.lock().unwrap();
        s.compiler_path = match s.compiler_path.as_deref() {
            None => Some("gcc".to_string()),
            Some("gcc") => Some("clang".to_string()),
            Some("clang") => Some("cc".to_string()),
            Some("cc") => None,
            Some(_) => None,
        };
    }

    fn on_mouse(&mut self, me: MouseEvent) {
        match me.kind {
            MouseEventKind::ScrollUp   => { self.scroll = self.scroll.saturating_add(3); }
            MouseEventKind::ScrollDown => { self.scroll = self.scroll.saturating_sub(3); }
            MouseEventKind::Down(MouseButton::Left) => {
                let (cols, rows) = terminal::size().unwrap_or((80, 24));
                let mx = me.column as usize;
                let my = me.row as usize;

                if self.show_flags {
                    let box_w: usize = 68;
                    let box_h: usize = 16;
                    let x_start = ((cols as usize).saturating_sub(box_w)) / 2;
                    let y_start = ((rows as usize).saturating_sub(box_h)) / 2;

                    if mx >= x_start && mx < x_start + box_w && my >= y_start && my < y_start + box_h {
                        let rel_y = my.saturating_sub(y_start);
                        let clicked_item = match rel_y {
                            1 | 2 => Some(0),
                            3 | 4 => Some(1),
                            5 | 6 => Some(2),
                            7 | 8 => Some(3),
                            9 | 10 => Some(4),
                            _ => None,
                        };
                        if let Some(item_idx) = clicked_item {
                            self.flags_selected_idx = item_idx;
                            self.toggle_flag_at(item_idx);
                        }
                    } else {
                        self.show_flags = false;
                    }
                } else if self.show_help {
                    self.show_help = false;
                } else {
                    // Click in output view selects line
                    let output_top: usize = 2;
                    let sep2_row: usize = (rows as usize).saturating_sub(3);

                    if my >= output_top && my < sep2_row {
                        let s = self.shared.lock().unwrap();
                        let total = s.lines.len();
                        let h = sep2_row.saturating_sub(output_top);

                        let end = total.saturating_sub(self.scroll);
                        let start = end.saturating_sub(h);
                        let empty_lines = h.saturating_sub(end - start);

                        if my >= output_top + empty_lines {
                            let slot = my.saturating_sub(output_top + empty_lines);
                            let clicked_idx = start + slot;

                            if clicked_idx < total {
                                if self.selected_line == Some(clicked_idx) {
                                    // Second click on same line copies to input prompt!
                                    drop(s);
                                    self.copy_selected_to_input();
                                } else {
                                    self.selected_line = Some(clicked_idx);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ── History helpers ─────────────────────────────────────────
    fn history_prev(&mut self) {
        if self.history.is_empty() { return; }
        match self.hist_idx {
            None    => { self.hist_save = self.input.clone(); self.hist_idx = Some(self.history.len() - 1); }
            Some(0) => {}
            Some(i) => { self.hist_idx = Some(i - 1); }
        }
        if let Some(i) = self.hist_idx {
            self.input = self.history[i].clone();
            self.cursor = self.input.len();
        }
    }

    fn history_next(&mut self) {
        match self.hist_idx {
            None => {}
            Some(i) if i + 1 >= self.history.len() => {
                self.hist_idx = None;
                self.input = self.hist_save.clone();
                self.cursor = self.input.len();
            }
            Some(i) => {
                self.hist_idx = Some(i + 1);
                self.input = self.history[i + 1].clone();
                self.cursor = self.input.len();
            }
        }
    }

    // ── Cursor / input helpers ──────────────────────────────────
    fn cursor_left(&mut self) {
        if self.cursor == 0 { return; }
        self.cursor -= 1;
        while !self.input.is_char_boundary(self.cursor) { self.cursor -= 1; }
    }
    fn cursor_right(&mut self) {
        if self.cursor >= self.input.len() { return; }
        self.cursor += 1;
        while !self.input.is_char_boundary(self.cursor) { self.cursor += 1; }
    }
    fn backspace(&mut self) {
        if self.cursor == 0 { return; }
        let mut new_cur = self.cursor - 1;
        while !self.input.is_char_boundary(new_cur) { new_cur -= 1; }
        self.input.drain(new_cur..self.cursor);
        self.cursor = new_cur;
    }
    fn delete_fwd(&mut self) {
        if self.cursor >= self.input.len() { return; }
        let mut end = self.cursor + 1;
        while !self.input.is_char_boundary(end) { end += 1; }
        self.input.drain(self.cursor..end);
    }

    // ── Execute ─────────────────────────────────────────────────
    fn execute(&self, cmd: String) {
        {
            let mut s = self.shared.lock().unwrap();
            s.lines.push(Line::new(format!("scm> {}", cmd), Kind::CmdEcho));
            s.running = true;
        }

        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            // Snapshot mutable state before releasing lock
            let (mut sess, quick, _auto_c, mem_i, runner, comp_path) = {
                let s = shared.lock().unwrap();
                (
                    s.session.clone(),
                    s.quick_mode,
                    s.auto_confirm,
                    s.mem_inspect,
                    s.runner_session,
                    s.compiler_path.clone(),
                )
            };

            // Run command; capture its stdout output
            let raw_output = capture_stdout(|| {
                crate::process_command(
                    &cmd,
                    &mut sess,
                    quick,
                    false,      // no editor mode in TUI
                    true,       // always auto-confirm in TUI (prevents blocking stdin reads)
                    mem_i,
                    comp_path.as_deref(),
                    runner,
                );
            });

            // Write session state back
            let mut s = shared.lock().unwrap();
            s.session.extra_headers = sess.extra_headers;
            s.session.extra_vars    = sess.extra_vars;
            s.session.statements    = sess.statements;
            s.session.focus_index   = sess.focus_index;

            // Parse output lines
            let mut out: Vec<Line> = raw_output
                .lines()
                .map(|l| Line::from_output(strip_ansi(l)))
                .collect();

            if out.is_empty() {
                out.push(Line::new("  (no output)", Kind::Normal));
            }
            out.push(Line::sep());
            s.push_lines(out);
            s.running = false;
        });
    }

    // ── Draw ─────────────────────────────────────────────────────
    fn draw(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        if rows < 4 { return Ok(()); }
        let s = self.shared.lock().unwrap();

        let header_row: u16 = 0;
        let sep1_row:   u16 = 1;
        let output_top: u16 = 2;
        let input_row:  u16 = rows - 2;
        let sep2_row:   u16 = rows - 3;
        let output_h:   u16 = sep2_row.saturating_sub(output_top);

        // ── Clear everything ────────────────────────────────────
        queue!(stdout, cursor::MoveTo(0, 0), SetBackgroundColor(BG))?;
        for r in 0..rows {
            queue!(stdout, cursor::MoveTo(0, r), Print(" ".repeat(cols as usize)))?;
        }

        // ── Header ─────────────────────────────────────────────
        self.draw_header(stdout, cols, header_row, &s)?;

        // ── Top separator ───────────────────────────────────────
        self.draw_hline(stdout, cols, sep1_row)?;

        // ── Output log ─────────────────────────────────────────
        self.draw_output(stdout, cols, output_top, output_h, &s)?;

        // ── Bottom separator ────────────────────────────────────
        self.draw_hline(stdout, cols, sep2_row)?;

        // ── Input bar ───────────────────────────────────────────
        self.draw_input(stdout, cols, input_row, s.running)?;

        // ── Overlays ────────────────────────────────────────────
        if self.show_flags {
            drop(s);
            self.draw_flags(stdout, cols, rows)?;
        } else if self.show_help {
            drop(s);
            self.draw_help(stdout, cols, rows)?;
        }

        Ok(())
    }

    fn draw_header(&self, stdout: &mut io::Stdout, cols: u16, row: u16, s: &TuiState) -> io::Result<()> {
        let mut active_flags = Vec::new();
        if s.runner_session { active_flags.push("-R"); }
        if s.quick_mode     { active_flags.push("-q"); }
        if s.mem_inspect    { active_flags.push("-m"); }
        if s.auto_confirm  { active_flags.push("-y"); }
        if let Some(ref comp) = s.compiler_path {
            active_flags.push(comp.as_str());
        }

        let flags_display = if active_flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", active_flags.join(" "))
        };

        let stmts = s.session.statements.len();
        let focus = match s.session.focus_index {
            Some(i) => format!("[{}]", i + 1),
            None    => "last".to_string(),
        };
        let run_indicator = if s.running { "  RUNNING…" } else { "" };
        let left  = format!(" SysCall Messenger TUI{flags_display} │ stmts:{stmts}  focus:{focus}{run_indicator}");
        let right = " F1=help  F2=flags  Ctrl+C=quit ";
        let pad   = (cols as usize).saturating_sub(left.chars().count() + right.chars().count());

        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(HEADER_FG),
            Print(&left),
            SetForegroundColor(DIM),
            Print(" ".repeat(pad)),
            Print(right),
            ResetColor,
        )?;
        Ok(())
    }

    fn draw_hline(&self, stdout: &mut io::Stdout, cols: u16, row: u16) -> io::Result<()> {
        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(BORDER),
            Print("─".repeat(cols as usize)),
            ResetColor,
        )?;
        Ok(())
    }

    fn draw_output(&self, stdout: &mut io::Stdout, cols: u16, top: u16, height: u16, s: &TuiState) -> io::Result<()> {
        let inner_w = cols as usize;
        let total   = s.lines.len();
        let h       = height as usize;

        // Compute visible window (newest at bottom, scroll moves window up)
        let end   = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(h);

        let empty_lines = h.saturating_sub(end - start);

        // Pad top with blank lines if content shorter than panel
        for r in top..top + empty_lines as u16 {
            queue!(stdout, cursor::MoveTo(0, r), SetBackgroundColor(BG), Print(" ".repeat(inner_w)))?;
        }

        for (slot, line_idx) in (start..end).enumerate() {
            let row = top + empty_lines as u16 + slot as u16;
            if row >= top + height { break; }

            let ol = &s.lines[line_idx];
            let is_selected = self.selected_line == Some(line_idx);

            // Separator → dim horizontal rule
            if ol.kind == Kind::Separator {
                let line_bg = if is_selected { SEL_BG } else { BG };
                queue!(stdout,
                    cursor::MoveTo(0, row),
                    SetBackgroundColor(line_bg),
                    SetForegroundColor(BORDER),
                    Print("─".repeat(inner_w)),
                )?;
                continue;
            }

            let default_color = match ol.kind {
                Kind::CmdEcho   => CMD_ECHO,
                Kind::KernelOk  => RESULT_OK,
                Kind::KernelErr => RESULT_ERR,
                Kind::Warning   => RESULT_WARN,
                Kind::Help      => DIM,
                _               => FG,
            };

            let (bg, fg) = if is_selected {
                (SEL_BG, Color::White)
            } else {
                (BG, default_color)
            };

            let prefix = if is_selected { "► " } else { "" };
            let full_text = format!("{}{}", prefix, ol.text);

            // Safe truncation to terminal width (character-aware, not byte-aware)
            let display: String = full_text.chars().take(inner_w).collect();
            let pad = inner_w.saturating_sub(display.chars().count());

            queue!(stdout,
                cursor::MoveTo(0, row),
                SetBackgroundColor(bg),
                SetForegroundColor(fg),
                Print(&display),
                Print(" ".repeat(pad)),
                ResetColor,
            )?;
        }

        Ok(())
    }

    fn draw_input(&self, stdout: &mut io::Stdout, cols: u16, row: u16, running: bool) -> io::Result<()> {
        let prompt = "scm> ";
        let prompt_len = prompt.chars().count();
        let avail = (cols as usize).saturating_sub(prompt_len + 1); // +1 for cursor block

        // If a line is selected, show hint in input area
        if let Some(sel) = self.selected_line {
            let hint = format!("[Line #{} selected  │  Enter/click=copy line  │  Esc=deselect]", sel + 1);
            let display: String = hint.chars().take(cols as usize).collect();
            let pad = (cols as usize).saturating_sub(display.chars().count());

            queue!(stdout,
                cursor::MoveTo(0, row),
                SetBackgroundColor(SEL_BG),
                SetForegroundColor(Color::White),
                Print(&display),
                Print(" ".repeat(pad)),
                ResetColor,
            )?;
            return Ok(());
        }

        // Find display window for input (keep cursor in view)
        let cur_char_pos: usize = self.input[..self.cursor].chars().count();
        let view_start_chars = if cur_char_pos >= avail { cur_char_pos - avail + 1 } else { 0 };

        // Convert char offset to byte offset
        let view_start_byte: usize = self.input
            .char_indices()
            .nth(view_start_chars)
            .map(|(b, _)| b)
            .unwrap_or(0);

        let visible_input: &str = &self.input[view_start_byte..];
        // Collect chars for safe width calculation
        let visible_chars: Vec<char> = visible_input.chars().take(avail + 1).collect();

        let cur_in_view = cur_char_pos - view_start_chars;

        queue!(stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(BG),
            SetForegroundColor(if running { RESULT_WARN } else { PROMPT }),
            Print(prompt),
            SetForegroundColor(FG),
        )?;

        for (i, &c) in visible_chars.iter().enumerate().take(avail + 1) {
            if i == cur_in_view {
                // Cursor block
                queue!(stdout,
                    SetBackgroundColor(FG),
                    SetForegroundColor(BG),
                    Print(c),
                    SetBackgroundColor(BG),
                    SetForegroundColor(FG),
                )?;
            } else if i < avail {
                queue!(stdout, Print(c))?;
            }
        }

        // If cursor is at end of input
        if cur_in_view >= visible_chars.len() {
            queue!(stdout,
                SetBackgroundColor(FG),
                SetForegroundColor(BG),
                Print(' '),
                SetBackgroundColor(BG),
                SetForegroundColor(FG),
            )?;
        }

        // Fill remainder
        let printed = visible_chars.len().min(avail);
        let pad = avail.saturating_sub(printed);
        if pad > 0 {
            queue!(stdout, Print(" ".repeat(pad)))?;
        }

        queue!(stdout, ResetColor)?;
        Ok(())
    }

    fn draw_flags(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        let s = self.shared.lock().unwrap();

        let compiler_name = match s.compiler_path.as_deref() {
            Some(c) => c.to_string(),
            None    => "auto (gcc/clang)".to_string(),
        };

        let items: [(&str, &str, &str, &str, bool); 5] = [
            ("1", "-R", "Persistent Runner Session", "Code space, state, FDs & variables persist across calls", s.runner_session),
            ("2", "-q", "Quick Execution Mode",  "Suppress C code space preview, execute instantly", s.quick_mode),
            ("3", "-m", "Memory Inspector Mode",  "Auto-detect and dump returned pointers/addresses", s.mem_inspect),
            ("4", "-y", "Auto-Confirm Mode",     "Automatically confirm prompts (e.g. headers/inspection)", s.auto_confirm),
            ("5", "-c", "C Compiler Binary",     &format!("Active: {}", compiler_name), s.compiler_path.is_some()),
        ];
        drop(s); // release lock before drawing

        let box_w: usize = 68;
        let box_h: usize = 16;
        let x = ((cols as usize).saturating_sub(box_w)) / 2;
        let y = ((rows as usize).saturating_sub(box_h)) / 2;

        // Draw top border
        queue!(stdout,
            cursor::MoveTo(x as u16, y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("┌─ "),
            SetForegroundColor(RESULT_OK),
            Print("Flags & Graphical Mode Configuration (F2 / f)"),
            SetForegroundColor(BORDER),
            Print(" "),
            Print("─".repeat((box_w - 2).saturating_sub(47))),
            Print("┐"),
        )?;

        // Content rows
        let mut cur_y = y + 1;
        for (i, (num, flag, name, desc, active)) in items.iter().enumerate() {
            let is_selected = self.flags_selected_idx == i;
            let check_mark = if i == 4 {
                format!("[{}]", compiler_name)
            } else if *active {
                "[X]".to_string()
            } else {
                "[ ]".to_string()
            };

            let prefix = if is_selected { " ► " } else { "   " };
            let title_line = format!("{}[{}] {} {:<4} {}", prefix, num, check_mark, flag, name);
            let desc_line  = format!("       {}", desc);

            let bg = if is_selected { Color::Rgb { r: 45, g: 45, b: 65 } } else { HEADER_BG };
            let title_fg = if is_selected { Color::White } else if *active { RESULT_OK } else { FG };
            let desc_fg  = if is_selected { Color::Rgb { r: 200, g: 200, b: 255 } } else { DIM };

            // Title row
            let display_title: String = format!("{:<width$}", title_line, width = box_w - 2).chars().take(box_w - 2).collect();
            queue!(stdout,
                cursor::MoveTo(x as u16, cur_y as u16),
                SetBackgroundColor(HEADER_BG),
                SetForegroundColor(BORDER),
                Print("│"),
                SetBackgroundColor(bg),
                SetForegroundColor(title_fg),
                Print(&display_title),
                SetBackgroundColor(HEADER_BG),
                SetForegroundColor(BORDER),
                Print("│"),
            )?;
            cur_y += 1;

            // Description row
            let display_desc: String = format!("{:<width$}", desc_line, width = box_w - 2).chars().take(box_w - 2).collect();
            queue!(stdout,
                cursor::MoveTo(x as u16, cur_y as u16),
                SetBackgroundColor(HEADER_BG),
                SetForegroundColor(BORDER),
                Print("│"),
                SetBackgroundColor(bg),
                SetForegroundColor(desc_fg),
                Print(&display_desc),
                SetBackgroundColor(HEADER_BG),
                SetForegroundColor(BORDER),
                Print("│"),
            )?;
            cur_y += 1;
        }

        // Blank line before footer
        queue!(stdout,
            cursor::MoveTo(x as u16, cur_y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("│"),
            Print(" ".repeat(box_w - 2)),
            Print("│"),
        )?;
        cur_y += 1;

        // Separator line
        queue!(stdout,
            cursor::MoveTo(x as u16, cur_y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("├"),
            Print("─".repeat(box_w - 2)),
            Print("┤"),
        )?;
        cur_y += 1;

        // Footer instructions
        let footer = " [▲/▼] Move  [Space/Enter] Toggle  [Left/Right/C] Compiler  [F2/Esc/q] Close ";
        let display_footer: String = format!("{:^width$}", footer, width = box_w - 2).chars().take(box_w - 2).collect();

        queue!(stdout,
            cursor::MoveTo(x as u16, cur_y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("│"),
            SetForegroundColor(RESULT_WARN),
            Print(&display_footer),
            SetForegroundColor(BORDER),
            Print("│"),
            ResetColor,
        )?;
        cur_y += 1;

        // Bottom border
        queue!(stdout,
            cursor::MoveTo(x as u16, cur_y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("└"),
            Print("─".repeat(box_w - 2)),
            Print("┘"),
            ResetColor,
        )?;

        Ok(())
    }

    fn draw_help(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        let entries: &[(&str, &str)] = &[
            ("KEYS",                 ""),
            ("  Enter",              "Execute command (or copy selected output line)"),
            ("  Up / Down",          "Command history  (when typing)"),
            ("  Up / Down",          "Select output lines (when input is empty)"),
            ("  PgUp / PgDn",        "Scroll output by page"),
            ("  Left / Right",       "Move cursor in input"),
            ("  Home / End",         "Jump to start / end of input"),
            ("  Backspace",          "Delete character before cursor"),
            ("  Delete",             "Delete character after cursor"),
            ("  Tab",                "Insert 4 spaces"),
            ("  Ctrl+L",             "Clear output panel"),
            ("  Mouse Click",        "Select output line / Toggle modal items"),
            ("  F1",                 "Show / hide this help"),
            ("  F2 or f",            "Flags & Modes Graphical Selection Modal"),
            ("  Esc",                "Deselect line / Close modal / Quit"),
            ("  Ctrl+C",             "Quit"),
            ("",                     ""),
            ("COMMANDS",             ""),
            ("  getpid()",           "Execute any syscall or libc function"),
            ("  cs",                 "Print current code space (C source)"),
            ("  cs run",             "Compile and run current code space"),
            ("  fc [L<n>|<n>]",      "Set focus point (only run up to that line)"),
            ("  rpc L<n> <code>",    "Replace line in code space"),
            ("  rpc L<n> NULL",      "Delete line from code space"),
            ("  rpc L*",             "Clear all lines"),
            ("  ins L<n> <code>",    "Insert new line at position"),
            ("  session",            "Show session state (headers, vars, stmts)"),
            ("  clr",                "Reset code space"),
            ("  exit  /  q",         "Quit"),
            ("",                     ""),
            ("  Press F1 or Esc to close", ""),
        ];

        let box_w: usize = 66;
        let box_h: usize = entries.len() + 2;
        let x = ((cols as usize).saturating_sub(box_w)) / 2;
        let y = ((rows as usize).saturating_sub(box_h)) / 2;

        queue!(stdout,
            cursor::MoveTo(x as u16, y as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("┌"),
            Print("─".repeat(box_w - 2)),
            Print("┐"),
        )?;

        for (i, (key, desc)) in entries.iter().enumerate() {
            let row = y + 1 + i;
            let is_section = desc.is_empty() && !key.is_empty() && !key.starts_with("  ");
            let is_blank   = key.is_empty() && desc.is_empty();

            let line_text = if is_blank {
                " ".repeat(box_w - 2)
            } else if is_section {
                let s = format!(" {}", key);
                format!("{:<width$}", s, width = box_w - 2)
            } else {
                let s = format!("{:<20}  {}", key, desc);
                format!("{:<width$}", s, width = box_w - 2)
            };

            let fg = if is_section { RESULT_WARN } else { FG };

            // Safe truncation to box width
            let display: String = line_text.chars().take(box_w - 2).collect();

            queue!(stdout,
                cursor::MoveTo(x as u16, row as u16),
                SetBackgroundColor(HEADER_BG),
                SetForegroundColor(BORDER),
                Print("│"),
                SetForegroundColor(fg),
                Print(&display),
                SetForegroundColor(BORDER),
                Print("│"),
            )?;
        }

        queue!(stdout,
            cursor::MoveTo(x as u16, (y + box_h - 1) as u16),
            SetBackgroundColor(HEADER_BG),
            SetForegroundColor(BORDER),
            Print("└"),
            Print("─".repeat(box_w - 2)),
            Print("┘"),
        )?;

        Ok(())
    }
}

// ── Strip ANSI escape codes ───────────────────────────────────
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it  = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            // consume until end of escape (letter ends most sequences)
            loop {
                match it.next() {
                    None        => break,
                    Some(ec) if ec.is_ascii_alphabetic() => break,
                    _ => {}
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Capture stdout from closure via pipe ─────────────────────
pub fn capture_stdout<F: FnOnce()>(f: F) -> String {
    unsafe {
        use libc::*;
        let mut pipefd: [c_int; 2] = [0; 2];
        if pipe(pipefd.as_mut_ptr()) != 0 {
            f();
            return String::new();
        }

        let saved_out = dup(1);
        let saved_err = dup(2);
        dup2(pipefd[1], 1);
        dup2(pipefd[1], 2);
        close(pipefd[1]);

        f();

        fflush(std::ptr::null_mut());

        dup2(saved_out, 1);
        dup2(saved_err, 2);
        close(saved_out);
        close(saved_err);

        // Read from pipe
        let mut buf = vec![0u8; 65536];
        let mut total = Vec::new();
        loop {
            let n = read(pipefd[0], buf.as_mut_ptr() as *mut c_void, buf.len());
            if n <= 0 { break; }
            total.extend_from_slice(&buf[..n as usize]);
        }
        close(pipefd[0]);

        String::from_utf8_lossy(&total).into_owned()
    }
}

// ── Public entry point ────────────────────────────────────────
pub fn run_tui(
    runner_session: bool,
    quick_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<String>,
) {
    let shared = Arc::new(Mutex::new(TuiState::new(
        runner_session, quick_mode, auto_confirm, mem_inspect, compiler_path,
    )));

    // Welcome text
    {
        let mut s = shared.lock().unwrap();
        s.lines.push(Line::new("  SysCall Messenger TUI", Kind::Normal));
        s.lines.push(Line::new("  Type any syscall or libc function and press Enter to execute it.", Kind::Normal));
        if runner_session {
            s.lines.push(Line::new("  Persistent session mode (-R): state, file descriptors, and variables persist.", Kind::Warning));
        }
        s.lines.push(Line::new("  Type 'cs' to view code space.  Type 'cs run' to compile and run.", Kind::Normal));
        s.lines.push(Line::new("  Click or use Up/Down to select lines. Press F1 for help, F2 for Flags.", Kind::Normal));
        s.lines.push(Line::sep());
    }

    let mut tui = Tui::new(Arc::clone(&shared));
    if let Err(e) = tui.run() {
        eprintln!("TUI error: {}", e);
    }
}
