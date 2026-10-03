// ============================================================
//  messenger.rs — Core messenger logic
//  Manages the FIFO channel, reading/writing messages, and
//  keeping a log of all syscall records.
// ============================================================

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libc::{O_NONBLOCK, O_RDONLY, O_WRONLY};

use crate::syscall_layer::{sc_mkfifo, sc_open, sc_poll, sc_read, sc_write, SyscallRecord};

pub const FIFO_PATH: &str = "/tmp/syscall_messenger.fifo";
const READ_BUF_SIZE: usize = 4096;

/// A single chat message
#[derive(Debug, Clone)]
pub struct Message {
    pub sender: String,
    pub body: String,
    pub timestamp: String,
    pub is_mine: bool,
}

/// Shared state between the UI thread and the reader thread
pub struct MessengerState {
    pub messages: Vec<Message>,
    pub syscall_log: Vec<SyscallRecord>,
    pub status: String,
    pub write_fd: Option<i32>,
    pub read_fd: Option<i32>,
    pub username: String,
}

impl MessengerState {
    pub fn new(username: String) -> Self {
        Self {
            messages: Vec::new(),
            syscall_log: Vec::new(),
            status: "Initialising channel…".to_string(),
            write_fd: None,
            read_fd: None,
            username,
        }
    }

    pub fn push_syscall(&mut self, rec: SyscallRecord) {
        self.syscall_log.push(rec);
        // Keep last 200 records to avoid unbounded growth
        if self.syscall_log.len() > 200 {
            self.syscall_log.remove(0);
        }
    }
}

pub type SharedState = Arc<Mutex<MessengerState>>;

/// Initialise the FIFO file and open both ends.
/// Returns (write_fd, read_fd) or an error string.
pub fn init_channel(state: &SharedState) -> Result<(i32, i32), String> {
    // ── Step 1: Create FIFO ──────────────────────────────────
    let (rec, result) = sc_mkfifo(FIFO_PATH);
    {
        let mut s = state.lock().unwrap();
        s.push_syscall(rec);
        if let Err(ref e) = result {
            // EEXIST is fine — the pipe already exists
            if !e.contains("File exists") {
                s.status = format!("mkfifo error: {}", e);
            }
        }
        s.status = "FIFO ready. Opening channel…".to_string();
    }

    // ── Step 2: Open read end FIRST (non-blocking) ──────────────
    // IMPORTANT: On Linux, opening O_WRONLY|O_NONBLOCK fails with ENXIO
    // unless a reader is already open. So we open the read end first.
    {
        let mut s = state.lock().unwrap();
        s.status = "FIFO created. Opening read end…".to_string();
    }
    let (rec_r, fd_r_res) = sc_open(FIFO_PATH, O_RDONLY | O_NONBLOCK);
    {
        let mut s = state.lock().unwrap();
        s.push_syscall(rec_r);
    }
    let read_fd = fd_r_res.map_err(|e| format!("Cannot open read end: {}", e))?;

    // ── Step 3: Open write end (reader is now open, so no ENXIO) ─
    {
        let mut s = state.lock().unwrap();
        s.status = "Read end open. Opening write end…".to_string();
    }
    let (rec_w, fd_w_res) = sc_open(FIFO_PATH, O_WRONLY | O_NONBLOCK);
    {
        let mut s = state.lock().unwrap();
        s.push_syscall(rec_w);
        s.status = "Channel open. Start chatting!".to_string();
    }
    let write_fd = fd_w_res.map_err(|e| format!("Cannot open write end: {}", e))?;

    Ok((write_fd, read_fd))
}

/// Send a message through the FIFO via sys_write.
pub fn send_message(state: &SharedState, text: String) {
    let (write_fd, username) = {
        let s = state.lock().unwrap();
        (s.write_fd, s.username.clone())
    };

    if let Some(fd) = write_fd {
        // Format: "USERNAME\x1F BODY\n"
        let packet = format!("{}\x1F{}\n", username, text);
        let bytes = packet.as_bytes();
        let (rec, result) = sc_write(fd, bytes);

        let mut s = state.lock().unwrap();
        s.push_syscall(rec);
        match result {
            Ok(_) => {
                let now = now_str();
                s.messages.push(Message {
                    sender: username.clone(),
                    body: text,
                    timestamp: now,
                    is_mine: true,
                });
                s.status = "Message sent via sys_write".to_string();
            }
            Err(e) => {
                s.status = format!("Send error: {}", e);
            }
        }
    } else {
        let mut s = state.lock().unwrap();
        s.status = "Channel not ready".to_string();
    }
}

/// Spawn a background thread that continuously polls and reads from the FIFO.
pub fn start_reader(state: SharedState) {
    thread::spawn(move || {
        loop {
            let read_fd = {
                let s = state.lock().unwrap();
                s.read_fd
            };

            if let Some(fd) = read_fd {
                // sys_poll — check if data is available
                let (poll_rec, ready) = sc_poll(fd, 50);
                {
                    let mut s = state.lock().unwrap();
                    s.push_syscall(poll_rec);
                }

                if ready {
                    let mut buf = [0u8; READ_BUF_SIZE];
                    let (read_rec, result) = sc_read(fd, &mut buf);
                    {
                        let mut s = state.lock().unwrap();
                        s.push_syscall(read_rec);
                        match result {
                            Ok(n) if n > 0 => {
                                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                                // Each line is one message packet
                                for line in raw.lines() {
                                    if let Some((sender, body)) = line.split_once('\x1F') {
                                        let is_mine = sender == s.username;
                                        if !is_mine {
                                            s.messages.push(Message {
                                                sender: sender.to_string(),
                                                body: body.to_string(),
                                                timestamp: now_str(),
                                                is_mine: false,
                                            });
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                s.status = format!("Read error: {}", e);
                            }
                            _ => {}
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis(50));
        }
    });
}

fn now_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
