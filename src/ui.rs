// ============================================================
//  ui.rs — Full-duplex TUI with toggleable syscall trace panel
//  Keys:
//    Enter       → send message
//    T           → toggle syscall trace panel
//    Esc / Ctrl+C → quit
// ============================================================

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};

use crate::messenger::{send_message, MessengerState, SharedState};

// ── Colour palette ───────────────────────────────────────────
const C_BG: Color = Color::Rgb { r: 10, g: 10, b: 20 };
const C_PANEL: Color = Color::Rgb { r: 18, g: 18, b: 32 };
const C_BORDER: Color = Color::Rgb { r: 80, g: 60, b: 200 };
const C_ACCENT: Color = Color::Rgb { r: 120, g: 80, b: 255 };
const C_MY_MSG: Color = Color::Rgb { r: 100, g: 220, b: 160 };
const C_THEIR_MSG: Color = Color::Rgb { r: 200, g: 160, b: 255 };
const C_TIMESTAMP: Color = Color::Rgb { r: 80, g: 80, b: 120 };
const C_SYS_NR: Color = Color::Rgb { r: 255, g: 180, b: 60 };
const C_SYS_NAME: Color = Color::Rgb { r: 60, g: 200, b: 255 };
const C_SYS_RESULT: Color = Color::Rgb { r: 100, g: 255, b: 100 };
const C_INPUT_BG: Color = Color::Rgb { r: 22, g: 22, b: 40 };
const C_WHITE: Color = Color::Rgb { r: 230, g: 230, b: 245 };
const C_DIM: Color = Color::Rgb { r: 90, g: 90, b: 120 };
const C_STATUS_OK: Color = Color::Rgb { r: 80, g: 220, b: 130 };
const C_STATUS_ERR: Color = Color::Rgb { r: 255, g: 90, b: 90 };

pub struct Tui {
    state: SharedState,
    input: String,
    show_trace: bool,
    scroll_offset: usize,
    trace_scroll: usize,
}

impl Tui {
    pub fn new(state: SharedState) -> Self {
        Self {
            state,
            input: String::new(),
            show_trace: false,
            scroll_offset: 0,
            trace_scroll: 0,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

        loop {
            let (cols, rows) = terminal::size()?;
            self.draw(&mut stdout, cols, rows)?;
            stdout.flush()?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key) {
                        break;
                    }
                }
            }
        }

        execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Returns true if user wants to quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            // Quit
            KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,

            // Toggle trace panel
            KeyCode::Char('t') | KeyCode::Char('T') => {
                self.show_trace = !self.show_trace;
            }

            // Send message
            KeyCode::Enter => {
                let text = self.input.trim().to_string();
                if !text.is_empty() {
                    send_message(&self.state, text);
                    self.input.clear();
                    // auto-scroll to bottom
                    self.scroll_offset = 0;
                }
            }

            // Input editing
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
            }

            // Scroll messages
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }

            // Scroll trace
            KeyCode::PageUp => {
                self.trace_scroll = self.trace_scroll.saturating_add(3);
            }
            KeyCode::PageDown => {
                self.trace_scroll = self.trace_scroll.saturating_sub(3);
            }

            _ => {}
        }
        false
    }

    fn draw(&self, stdout: &mut io::Stdout, cols: u16, rows: u16) -> io::Result<()> {
        let s = self.state.lock().unwrap();

        // Layout
        let trace_width: u16 = if self.show_trace { cols / 3 } else { 0 };
        let chat_width: u16 = cols - trace_width;
        let header_h: u16 = 3;
        let input_h: u16 = 3;
        let status_h: u16 = 1;
        let msg_area_h = rows.saturating_sub(header_h + input_h + status_h);

        // Clear
        queue!(stdout, cursor::MoveTo(0, 0))?;
        for r in 0..rows {
            queue!(
                stdout,
                cursor::MoveTo(0, r),
                SetBackgroundColor(C_BG),
                Print(" ".repeat(cols as usize))
            )?;
        }

        // ── Header ──────────────────────────────────────────
        self.draw_header(stdout, cols, &s)?;

        // ── Messages panel ──────────────────────────────────
        self.draw_messages(stdout, chat_width, header_h, msg_area_h, &s)?;

        // ── Syscall trace panel ──────────────────────────────
        if self.show_trace {
            self.draw_trace(stdout, chat_width, trace_width, header_h, msg_area_h + input_h, &s)?;
        }

        // ── Input box ───────────────────────────────────────
        let input_row = header_h + msg_area_h;
        self.draw_input(stdout, chat_width, input_row, input_h)?;

        // ── Status bar ──────────────────────────────────────
        let status_row = rows - 1;
        self.draw_status(stdout, cols, status_row, &s)?;

        Ok(())
    }

    // ────────────────────────────────────────────────────────
    fn draw_header(&self, stdout: &mut io::Stdout, cols: u16, s: &MessengerState) -> io::Result<()> {
        // Top border line
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╔"),
            Print("═".repeat(cols as usize - 2)),
            Print("╗"),
        )?;

        // Title row
        let title = format!(
            " 🔬 SysCall Messenger  ·  User: {}  ·  FIFO: /tmp/syscall_messenger.fifo ",
            s.username
        );
        let hint = if self.show_trace { " [T] Hide Trace " } else { " [T] Show Trace " };
        let padding = (cols as usize).saturating_sub(title.len() + hint.len() + 2);

        queue!(
            stdout,
            cursor::MoveTo(0, 1),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("║"),
            SetForegroundColor(C_ACCENT),
            Print(&title),
            SetForegroundColor(C_DIM),
            Print(" ".repeat(padding)),
            SetForegroundColor(C_SYS_NR),
            Print(&hint),
            SetForegroundColor(C_BORDER),
            Print("║"),
        )?;

        // Bottom border
        queue!(
            stdout,
            cursor::MoveTo(0, 2),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╠"),
            Print("═".repeat(cols as usize - 2)),
            Print("╣"),
        )?;

        Ok(())
    }

    // ────────────────────────────────────────────────────────
    fn draw_messages(
        &self,
        stdout: &mut io::Stdout,
        width: u16,
        top: u16,
        height: u16,
        s: &MessengerState,
    ) -> io::Result<()> {
        let inner_w = width.saturating_sub(2) as usize;
        let msgs = &s.messages;

        // Build rendered lines (newest last)
        let mut lines: Vec<(bool, String, String, String)> = Vec::new();
        for msg in msgs {
            lines.push((msg.is_mine, msg.sender.clone(), msg.body.clone(), msg.timestamp.clone()));
        }

        let total = lines.len();
        let visible = height as usize;
        let start = if total > visible + self.scroll_offset {
            total - visible - self.scroll_offset
        } else {
            0
        };
        let end = (start + visible).min(total);

        for (i, row) in (top..top + height).enumerate() {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                SetBackgroundColor(C_PANEL),
                SetForegroundColor(C_BORDER),
                Print("║"),
            )?;

            let line_idx = start + i;
            if line_idx < end {
                let (is_mine, sender, body, ts) = &lines[line_idx];

                if *is_mine {
                    // Right-align mine
                    let tag = format!("[{}]", sender);
                    let body_max = inner_w.saturating_sub(tag.len() + ts.len() + 3);
                    let body_display = if body.len() > body_max {
                        &body[..body_max]
                    } else {
                        body.as_str()
                    };
                    let content = format!("{} {} {}", ts, body_display, tag);
                    let pad = inner_w.saturating_sub(content.len());

                    queue!(
                        stdout,
                        SetBackgroundColor(C_PANEL),
                        SetForegroundColor(C_TIMESTAMP),
                        Print(" ".repeat(pad + 1)),
                        Print(ts),
                        Print(" "),
                        SetForegroundColor(C_MY_MSG),
                        Print(body_display),
                        Print(" "),
                        SetForegroundColor(C_ACCENT),
                        Print(&tag),
                    )?;
                } else {
                    // Left-align theirs
                    let tag = format!("[{}]", sender);
                    let body_max = inner_w.saturating_sub(tag.len() + ts.len() + 3);
                    let body_display = if body.len() > body_max {
                        &body[..body_max]
                    } else {
                        body.as_str()
                    };
                    let content = format!("{} {} {}", tag, body_display, ts);
                    let pad = inner_w.saturating_sub(content.len());

                    queue!(
                        stdout,
                        SetBackgroundColor(C_PANEL),
                        SetForegroundColor(C_SYS_NAME),
                        Print(" "),
                        Print(&tag),
                        Print(" "),
                        SetForegroundColor(C_THEIR_MSG),
                        Print(body_display),
                        Print(" "),
                        SetForegroundColor(C_TIMESTAMP),
                        Print(ts),
                        Print(" ".repeat(pad)),
                    )?;
                }
            } else {
                queue!(
                    stdout,
                    SetBackgroundColor(C_PANEL),
                    Print(" ".repeat(inner_w + 1)),
                )?;
            }

            queue!(
                stdout,
                SetForegroundColor(C_BORDER),
                Print("║"),
            )?;
        }

        Ok(())
    }

    // ────────────────────────────────────────────────────────
    fn draw_input(&self, stdout: &mut io::Stdout, width: u16, top: u16, _height: u16) -> io::Result<()> {
        let inner_w = width.saturating_sub(4) as usize;

        // Top border
        queue!(
            stdout,
            cursor::MoveTo(0, top),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╠"),
            Print("═".repeat(width as usize - 2)),
            Print("╣"),
        )?;

        // Input line
        let prompt = " ▶ ";
        let max_input = inner_w.saturating_sub(prompt.len());
        let display_input = if self.input.len() > max_input {
            &self.input[self.input.len() - max_input..]
        } else {
            &self.input
        };
        let cursor_char = "█";
        let pad = inner_w.saturating_sub(prompt.len() + display_input.len() + 1);

        queue!(
            stdout,
            cursor::MoveTo(0, top + 1),
            SetBackgroundColor(C_INPUT_BG),
            SetForegroundColor(C_BORDER),
            Print("║"),
            SetForegroundColor(C_ACCENT),
            Print(prompt),
            SetForegroundColor(C_WHITE),
            Print(display_input),
            SetForegroundColor(C_ACCENT),
            Print(cursor_char),
            SetForegroundColor(C_DIM),
            Print(" ".repeat(pad)),
            SetForegroundColor(C_BORDER),
            Print("║"),
        )?;

        // Bottom border
        queue!(
            stdout,
            cursor::MoveTo(0, top + 2),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╚"),
            Print("═".repeat(width as usize - 2)),
            Print("╝"),
        )?;

        Ok(())
    }

    // ────────────────────────────────────────────────────────
    fn draw_trace(
        &self,
        stdout: &mut io::Stdout,
        x_off: u16,
        width: u16,
        top: u16,
        height: u16,
        s: &MessengerState,
    ) -> io::Result<()> {
        let inner_w = width.saturating_sub(2) as usize;
        let log = &s.syscall_log;
        let total = log.len();
        let visible = height as usize / 4; // each entry takes ~4 lines
        let start = if total > visible + self.trace_scroll {
            total - visible - self.trace_scroll
        } else {
            0
        };

        // Header
        queue!(
            stdout,
            cursor::MoveTo(x_off, top),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╦"),
            Print("═".repeat(width as usize - 1)),
        )?;

        let title = " 🛸 Syscall Trace [PgUp/PgDn] ";
        queue!(
            stdout,
            cursor::MoveTo(x_off, top + 1),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("║"),
            SetForegroundColor(C_SYS_NR),
            Print(title),
            SetForegroundColor(C_DIM),
            Print(" ".repeat(inner_w.saturating_sub(title.len()))),
        )?;

        queue!(
            stdout,
            cursor::MoveTo(x_off, top + 2),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(C_BORDER),
            Print("╠"),
            Print("═".repeat(width as usize - 1)),
        )?;

        let mut row = top + 3;
        let max_row = top + height;

        for rec in log.iter().skip(start) {
            if row >= max_row {
                break;
            }

            // Line 1: syscall nr + name + result
            let line1 = format!(
                " #{:<3} {} → {}",
                rec.number, rec.name, rec.result
            );
            let pad1 = inner_w.saturating_sub(line1.len() + rec.timestamp.len() + 1);
            queue!(
                stdout,
                cursor::MoveTo(x_off, row),
                SetBackgroundColor(C_PANEL),
                SetForegroundColor(C_BORDER),
                Print("║"),
                SetForegroundColor(C_SYS_NR),
                Print(format!(" #{:<3} ", rec.number)),
                SetForegroundColor(C_SYS_NAME),
                Print(&rec.name),
                SetForegroundColor(C_DIM),
                Print(" → "),
                SetForegroundColor(C_SYS_RESULT),
                Print(format!("{}", rec.result)),
                Print(" ".repeat(pad1)),
                SetForegroundColor(C_TIMESTAMP),
                Print(&rec.timestamp),
                Print(" "),
            )?;
            row += 1;
            if row >= max_row { break; }

            // Lines 2+: asm snippet (first 3 lines)
            for asm_line in rec.asm_snippet.lines().take(3) {
                let trimmed = if asm_line.len() > inner_w - 2 {
                    &asm_line[..inner_w - 2]
                } else {
                    asm_line
                };
                let pad = inner_w.saturating_sub(trimmed.len() + 2);
                queue!(
                    stdout,
                    cursor::MoveTo(x_off, row),
                    SetBackgroundColor(C_PANEL),
                    SetForegroundColor(C_BORDER),
                    Print("║"),
                    SetForegroundColor(C_DIM),
                    Print("  "),
                    SetForegroundColor(C_WHITE),
                    Print(trimmed),
                    Print(" ".repeat(pad)),
                )?;
                row += 1;
                if row >= max_row { break; }
            }

            // Separator
            if row < max_row {
                queue!(
                    stdout,
                    cursor::MoveTo(x_off, row),
                    SetBackgroundColor(C_PANEL),
                    SetForegroundColor(C_BORDER),
                    Print("║"),
                    SetForegroundColor(C_DIM),
                    Print("─".repeat(inner_w)),
                )?;
                row += 1;
            }
        }

        // Fill remaining rows
        while row < max_row {
            queue!(
                stdout,
                cursor::MoveTo(x_off, row),
                SetBackgroundColor(C_PANEL),
                SetForegroundColor(C_BORDER),
                Print("║"),
                Print(" ".repeat(inner_w)),
            )?;
            row += 1;
        }

        Ok(())
    }

    // ────────────────────────────────────────────────────────
    fn draw_status(&self, stdout: &mut io::Stdout, cols: u16, row: u16, s: &MessengerState) -> io::Result<()> {
        let status = &s.status;
        let is_err = status.to_lowercase().contains("error");
        let color = if is_err { C_STATUS_ERR } else { C_STATUS_OK };
        let msg_count = format!("  msgs:{} calls:{} ", s.messages.len(), s.syscall_log.len());
        let controls = " ↑↓ scroll · Enter send · T trace · Esc quit ";
        let status_display = format!("  {}", status);
        let pad = (cols as usize)
            .saturating_sub(status_display.len() + msg_count.len() + controls.len());

        queue!(
            stdout,
            cursor::MoveTo(0, row),
            SetBackgroundColor(C_PANEL),
            SetForegroundColor(color),
            Print(&status_display),
            SetForegroundColor(C_DIM),
            Print(" ".repeat(pad)),
            SetForegroundColor(C_TIMESTAMP),
            Print(&msg_count),
            SetForegroundColor(C_DIM),
            Print(&controls),
            ResetColor,
        )?;

        Ok(())
    }
}
