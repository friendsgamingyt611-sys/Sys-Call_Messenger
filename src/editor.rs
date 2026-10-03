// ============================================================
//  editor.rs — Built-in interactive terminal C code editor
//
//  Modes:
//    OVR (default) — typing overwrites the character at cursor
//    INS           — typing inserts before cursor
//    Switch:  i → INS,  o → OVR
//
//  Key bindings:
//    Ctrl+S / F2      Save & Run immediately (no second prompt)
//    Esc / Ctrl+Q     Cancel
//    i                Switch to INSERT mode   (blinking bar cursor)
//    o                Switch to OVERWRITE mode (solid block cursor)
//    Delete           Delete the entire current line
//    Ctrl+C           Copy current line to internal clipboard
//    Ctrl+V           Paste clipboard line(s) below cursor
//    Backspace        Delete char left (both modes)
//    Enter            Split line / new line
//    Tab              Insert 4 spaces
//    Arrow keys       Move cursor
//    Home / End       Start / end of line
//    PageUp / PgDn    Page scroll
//    Mouse wheel      Scroll viewport (terminal emulator unaffected)
// ============================================================

use crossterm::{
    cursor::{self, SetCursorStyle},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
        MouseEvent, MouseEventKind,
    },
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use std::io::{stdout, Write};

// ── Editor mode ──────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
pub enum EditMode {
    Insert,
    Overwrite,
}

impl EditMode {
    fn label(self) -> &'static str {
        match self {
            EditMode::Insert    => "INS",
            EditMode::Overwrite => "OVR",
        }
    }
}

pub enum EditorResult {
    Save(String),
    Cancel,
}

pub struct ScmEditor {
    pub lines:         Vec<String>,
    pub cursor_row:    usize,
    pub cursor_col:    usize,
    pub scroll_offset: usize,
    pub mode:          EditMode,
    /// Internal clipboard — holds copied lines (Ctrl+C / Ctrl+V)
    pub clipboard:     Vec<String>,
}

impl ScmEditor {
    pub fn new(initial_code: &str) -> Self {
        let lines: Vec<String> = if initial_code.is_empty() {
            vec![String::new()]
        } else {
            initial_code.lines().map(|s| s.to_string()).collect()
        };
        Self {
            lines,
            cursor_row:    0,
            cursor_col:    0,
            scroll_offset: 0,
            mode:          EditMode::Overwrite, // OVR is default
            clipboard:     Vec::new(),
        }
    }

    pub fn run(&mut self) -> EditorResult {
        let _ = terminal::enable_raw_mode();
        let mut stdout = stdout();
        // EnterAlternateScreen isolates our UI from the terminal scrollback.
        // EnableMouseCapture intercepts scroll-wheel events so they move our
        // viewport instead of scrolling the terminal emulator's buffer.
        let _ = execute!(
            stdout,
            terminal::EnterAlternateScreen,
            EnableMouseCapture,
            cursor::Show,
            // OVR default = solid block cursor
            SetCursorStyle::SteadyBlock
        );

        let result = self.editor_loop(&mut stdout);

        // Always restore terminal state, even on panic/cancel.
        let _ = execute!(
            stdout,
            DisableMouseCapture,
            terminal::LeaveAlternateScreen,
            cursor::Show,
            // Restore shell default cursor shape
            SetCursorStyle::DefaultUserShape
        );
        let _ = stdout.flush();
        let _ = terminal::disable_raw_mode();

        result
    }

    // ── Main event loop ──────────────────────────────────────
    fn editor_loop(&mut self, stdout: &mut std::io::Stdout) -> EditorResult {
        loop {
            self.draw(stdout);

            match event::read() {
                // ── Keyboard ─────────────────────────────────
                Ok(Event::Key(key)) => match (key.code, key.modifiers) {

                    // Save & Run: Ctrl+S, F2
                    (KeyCode::Char('s'), KeyModifiers::CONTROL)
                    | (KeyCode::F(2), _) => {
                        let content = self.lines.join("\n");
                        return EditorResult::Save(content);
                    }

                    // Cancel: Esc, Ctrl+Q
                    (KeyCode::Esc, _)
                    | (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                        return EditorResult::Cancel;
                    }

                    // Copy current line: Ctrl+C
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        self.clipboard = vec![self.lines[self.cursor_row].clone()];
                    }

                    // Paste below cursor: Ctrl+V
                    (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                        if !self.clipboard.is_empty() {
                            let insert_at = self.cursor_row + 1;
                            for (i, line) in self.clipboard.iter().enumerate() {
                                self.lines.insert(insert_at + i, line.clone());
                            }
                            self.cursor_row = insert_at;
                            self.cursor_col = self.cursor_col
                                .min(self.lines[self.cursor_row].len());
                            self.adjust_scroll();
                        }
                    }

                    // Mode switching — only without modifiers
                    (KeyCode::Char('i'), KeyModifiers::NONE) => {
                        self.mode = EditMode::Insert;
                        // Blinking bar = INSERT mode (universal convention)
                        let _ = execute!(stdout, SetCursorStyle::BlinkingBar);
                    }
                    (KeyCode::Char('o'), KeyModifiers::NONE) => {
                        self.mode = EditMode::Overwrite;
                        // Solid block = OVERWRITE mode
                        let _ = execute!(stdout, SetCursorStyle::SteadyBlock);
                    }

                    // ── Navigation ────────────────────────────
                    (KeyCode::Up, _) => {
                        if self.cursor_row > 0 {
                            self.cursor_row -= 1;
                            self.cursor_col =
                                self.cursor_col.min(self.lines[self.cursor_row].len());
                            self.adjust_scroll();
                        }
                    }
                    (KeyCode::Down, _) => {
                        if self.cursor_row + 1 < self.lines.len() {
                            self.cursor_row += 1;
                            self.cursor_col =
                                self.cursor_col.min(self.lines[self.cursor_row].len());
                            self.adjust_scroll();
                        }
                    }
                    (KeyCode::PageUp, _) => {
                        let (_, height) = terminal::size().unwrap_or((80, 24));
                        let page = height.saturating_sub(3) as usize;
                        self.cursor_row = self.cursor_row.saturating_sub(page);
                        self.cursor_col =
                            self.cursor_col.min(self.lines[self.cursor_row].len());
                        self.adjust_scroll();
                    }
                    (KeyCode::PageDown, _) => {
                        let (_, height) = terminal::size().unwrap_or((80, 24));
                        let page = height.saturating_sub(3) as usize;
                        self.cursor_row = (self.cursor_row + page)
                            .min(self.lines.len().saturating_sub(1));
                        self.cursor_col =
                            self.cursor_col.min(self.lines[self.cursor_row].len());
                        self.adjust_scroll();
                    }
                    (KeyCode::Left, _) => {
                        if self.cursor_col > 0 {
                            self.cursor_col -= 1;
                        } else if self.cursor_row > 0 {
                            self.cursor_row -= 1;
                            self.cursor_col = self.lines[self.cursor_row].len();
                            self.adjust_scroll();
                        }
                    }
                    (KeyCode::Right, _) => {
                        if self.cursor_col < self.lines[self.cursor_row].len() {
                            self.cursor_col += 1;
                        } else if self.cursor_row + 1 < self.lines.len() {
                            self.cursor_row += 1;
                            self.cursor_col = 0;
                            self.adjust_scroll();
                        }
                    }
                    (KeyCode::Home, _) => {
                        self.cursor_col = 0;
                    }
                    (KeyCode::End, _) => {
                        self.cursor_col = self.lines[self.cursor_row].len();
                    }

                    // ── Editing ───────────────────────────────

                    // Delete key → remove the ENTIRE current line
                    (KeyCode::Delete, _) => {
                        if self.lines.len() > 1 {
                            self.lines.remove(self.cursor_row);
                            if self.cursor_row >= self.lines.len() {
                                self.cursor_row = self.lines.len().saturating_sub(1);
                            }
                            self.cursor_col =
                                self.cursor_col.min(self.lines[self.cursor_row].len());
                            self.adjust_scroll();
                        } else {
                            // Only one line left — clear it
                            self.lines[0].clear();
                            self.cursor_col = 0;
                        }
                    }

                    // Backspace — delete char left (same in both modes)
                    (KeyCode::Backspace, _) => {
                        if self.cursor_col > 0 {
                            self.lines[self.cursor_row].remove(self.cursor_col - 1);
                            self.cursor_col -= 1;
                        } else if self.cursor_row > 0 {
                            let prev_len = self.lines[self.cursor_row - 1].len();
                            let current_line = self.lines.remove(self.cursor_row);
                            self.cursor_row -= 1;
                            self.lines[self.cursor_row].push_str(&current_line);
                            self.cursor_col = prev_len;
                            self.adjust_scroll();
                        }
                    }

                    // Enter — split line (same in both modes)
                    (KeyCode::Enter, _) => {
                        let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
                        self.cursor_row += 1;
                        self.lines.insert(self.cursor_row, rest);
                        self.cursor_col = 0;
                        self.adjust_scroll();
                    }

                    // Tab — 4 spaces (same in both modes)
                    (KeyCode::Tab, _) => {
                        let row = self.cursor_row;
                        let col = self.cursor_col;
                        self.lines[row].insert_str(col, "    ");
                        self.cursor_col += 4;
                    }

                    // Character input — INS vs OVR behaviour
                    (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                        let row = self.cursor_row;
                        let col = self.cursor_col;
                        match self.mode {
                            EditMode::Insert => {
                                // Push chars right
                                self.lines[row].insert(col, c);
                                self.cursor_col += 1;
                            }
                            EditMode::Overwrite => {
                                let line_len = self.lines[row].len();
                                if col < line_len {
                                    // Replace char in place
                                    self.lines[row].remove(col);
                                    self.lines[row].insert(col, c);
                                } else {
                                    // At/past end of line — append
                                    self.lines[row].push(c);
                                }
                                self.cursor_col += 1;
                            }
                        }
                    }

                    _ => {}
                },

                // ── Mouse scroll — captured so terminal emulator never sees it
                Ok(Event::Mouse(MouseEvent { kind, .. })) => match kind {
                    MouseEventKind::ScrollUp => {
                        if self.scroll_offset > 0 {
                            self.scroll_offset -= 1;
                            let (_, height) = terminal::size().unwrap_or((80, 24));
                            let visible_rows = height.saturating_sub(3) as usize;
                            if self.cursor_row >= self.scroll_offset + visible_rows {
                                self.cursor_row = self.scroll_offset + visible_rows - 1;
                                self.cursor_col =
                                    self.cursor_col.min(self.lines[self.cursor_row].len());
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        let (_, height) = terminal::size().unwrap_or((80, 24));
                        let visible_rows = height.saturating_sub(3) as usize;
                        let max_scroll = self.lines.len().saturating_sub(1);
                        if self.scroll_offset < max_scroll {
                            self.scroll_offset += 1;
                            if self.cursor_row < self.scroll_offset {
                                self.cursor_row = self.scroll_offset;
                                self.cursor_col =
                                    self.cursor_col.min(self.lines[self.cursor_row].len());
                            } else if self.cursor_row >= self.scroll_offset + visible_rows {
                                self.cursor_row = self.scroll_offset + visible_rows - 1;
                                self.cursor_col =
                                    self.cursor_col.min(self.lines[self.cursor_row].len());
                            }
                        }
                    }
                    _ => {}
                },

                // Ignore resize and other events
                _ => {}
            }
        }
    }

    // ── Scroll adjustment ─────────────────────────────────────
    fn adjust_scroll(&mut self) {
        let (_, height) = terminal::size().unwrap_or((80, 24));
        let visible_rows = height.saturating_sub(3) as usize;
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.cursor_row + 1 - visible_rows;
        }
    }

    // ── Rendering ─────────────────────────────────────────────
    fn draw(&self, stdout: &mut std::io::Stdout) {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        // 3 reserved rows: header (0), status (h-2), footer (h-1)
        let visible_rows = height.saturating_sub(3) as usize;

        let _ = execute!(stdout, Clear(ClearType::All));

        let total      = self.lines.len();
        let mode_label = self.mode.label();

        // ── Row 0: Header ─────────────────────────────────────
        let clip_badge = if self.clipboard.is_empty() { "" } else { " 📋" };
        let header = format!(
            " ✏  SCM Editor  │  Ln {}/{}  │  {}{}  │  Ctrl+S → Run  │  Esc → Cancel ",
            self.cursor_row + 1, total, mode_label, clip_badge,
        );
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, 0),
            SetBackgroundColor(Color::Rgb { r: 22, g: 22, b: 70 }),
            SetForegroundColor(Color::Rgb { r: 195, g: 215, b: 255 }),
            Print(format!("{:<width$}", header, width = width as usize)),
            ResetColor
        );

        // ── Rows 1..=visible_rows: Code ───────────────────────
        let gutter_width   = format!("{}", total).len().max(3);
        let code_start_col = (gutter_width + 3) as u16; // " NNN │ "

        // Current-line colours differ by mode
        let (cur_bg, cur_fg, gutter_fg) = match self.mode {
            EditMode::Insert => (
                Color::Rgb { r: 22, g: 48, b: 32 },
                Color::Rgb { r: 205, g: 255, b: 205 },
                Color::Rgb { r: 90,  g: 240, b: 120 },
            ),
            EditMode::Overwrite => (
                Color::Rgb { r: 32, g: 32, b: 58 },
                Color::Rgb { r: 228, g: 228, b: 255 },
                Color::Rgb { r: 255, g: 195, b: 70 },
            ),
        };

        for screen_row in 0..visible_rows {
            let file_row = self.scroll_offset + screen_row;
            let term_row = (screen_row + 1) as u16;

            // CRITICAL: explicit MoveTo per line — raw mode doesn't CR on \n
            let _ = execute!(stdout, cursor::MoveTo(0, term_row));

            if file_row < total {
                let line_num = file_row + 1;
                let line_str = &self.lines[file_row];
                let is_cur   = file_row == self.cursor_row;

                // Gutter
                if is_cur {
                    let _ = execute!(
                        stdout,
                        SetBackgroundColor(cur_bg),
                        SetForegroundColor(gutter_fg),
                        Print(format!(" {:>gw$} │", line_num, gw = gutter_width)),
                        ResetColor
                    );
                } else {
                    let _ = execute!(
                        stdout,
                        SetForegroundColor(Color::Rgb { r: 75, g: 75, b: 110 }),
                        Print(format!(" {:>gw$} │", line_num, gw = gutter_width)),
                        ResetColor
                    );
                }

                // Code content
                let max_code_width =
                    (width as usize).saturating_sub(code_start_col as usize + 1);
                let display_str: String = line_str.chars().take(max_code_width).collect();

                if is_cur {
                    let _ = execute!(
                        stdout,
                        SetBackgroundColor(cur_bg),
                        SetForegroundColor(cur_fg),
                        Print(format!("{:<width$}", display_str, width = max_code_width)),
                        ResetColor
                    );
                } else {
                    let _ = execute!(
                        stdout,
                        SetForegroundColor(Color::Rgb { r: 172, g: 192, b: 212 }),
                        Print(display_str),
                        ResetColor
                    );
                }
            } else {
                // vim-style tilde for empty rows past EOF
                let _ = execute!(
                    stdout,
                    SetForegroundColor(Color::Rgb { r: 60, g: 60, b: 90 }),
                    Print(format!(" {:>gw$} │ ~", "", gw = gutter_width)),
                    ResetColor
                );
            }
        }

        // ── Status bar (second to last row) ───────────────────
        let status_y  = height.saturating_sub(2);
        let clip_info = if self.clipboard.is_empty() {
            String::new()
        } else {
            format!("  │  📋 {} line(s) ready to paste", self.clipboard.len())
        };
        let status = format!(
            "  Col {}  │  {} lines  │  {}{}",
            self.cursor_col + 1, total, mode_label, clip_info,
        );
        let (sb_bg, sb_fg) = match self.mode {
            EditMode::Insert => (
                Color::Rgb { r: 15, g: 50, b: 25 },
                Color::Rgb { r: 120, g: 225, b: 145 },
            ),
            EditMode::Overwrite => (
                Color::Rgb { r: 18, g: 18, b: 55 },
                Color::Rgb { r: 115, g: 135, b: 200 },
            ),
        };
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, status_y),
            SetBackgroundColor(sb_bg),
            SetForegroundColor(sb_fg),
            Print(format!("{:<width$}", status, width = width as usize)),
            ResetColor
        );

        // ── Footer bar (last row) ─────────────────────────────
        let footer = " [Ctrl+S] Run  [i] INS  [o] OVR  [Del] Del Line  [Ctrl+C] Copy  [Ctrl+V] Paste  [Esc] Cancel ";
        let footer_y = height.saturating_sub(1);
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, footer_y),
            SetBackgroundColor(Color::Rgb { r: 12, g: 65, b: 30 }),
            SetForegroundColor(Color::Rgb { r: 155, g: 255, b: 155 }),
            Print(format!("{:<width$}", footer, width = width as usize)),
            ResetColor
        );

        // ── Place terminal cursor at editing position ─────────
        let vis_row  = self.cursor_row.saturating_sub(self.scroll_offset);
        let term_col = code_start_col + self.cursor_col as u16;
        let term_row = (vis_row + 1) as u16; // +1 for header row
        let _ = execute!(stdout, cursor::MoveTo(term_col, term_row));
        let _ = stdout.flush();
    }
}
