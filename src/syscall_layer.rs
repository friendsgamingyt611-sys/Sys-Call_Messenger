// ============================================================
//  syscall_layer.rs — Raw Linux syscall wrappers
//  Every I/O operation goes through these functions so we can
//  log the exact syscall number and register values.
// ============================================================

use libc::{
    c_int, c_void, mode_t, nfds_t, size_t, POLLIN, S_IFIFO,
};

use std::ffi::CString;

// ── Syscall numbers on x86-64 Linux ─────────────────────────
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_POLL: u64 = 7;
pub const SYS_MKNOD: u64 = 133;
pub const SYS_STAT: u64 = 4;

/// A record of one syscall invocation.
#[derive(Debug, Clone)]
pub struct SyscallRecord {
    pub number: u64,
    pub name: &'static str,
    #[allow(dead_code)]
    pub args: Vec<String>,
    pub asm_snippet: String,
    pub result: i64,
    pub timestamp: String,
}

impl SyscallRecord {
    fn new(
        number: u64,
        name: &'static str,
        args: Vec<String>,
        result: i64,
        timestamp: String,
    ) -> Self {
        // Build a representative x86-64 asm snippet for display
        let asm_snippet = Self::build_asm(number, &args);
        Self {
            number,
            name,
            args,
            asm_snippet,
            result,
            timestamp,
        }
    }

    fn build_asm(nr: u64, args: &[String]) -> String {
        let regs = ["rdi", "rsi", "rdx", "r10", "r8", "r9"];
        let mut lines = vec![format!("mov rax, {:#x}  ; {}", nr, syscall_name(nr))];
        for (i, arg) in args.iter().enumerate().take(6) {
            lines.push(format!("mov {}, {}  ; arg{}", regs[i], arg, i + 1));
        }
        lines.push("syscall".to_string());
        lines.join("\n")
    }
}

fn syscall_name(nr: u64) -> &'static str {
    match nr {
        SYS_READ => "sys_read",
        SYS_WRITE => "sys_write",
        SYS_OPEN => "sys_open",
        SYS_CLOSE => "sys_close",
        SYS_POLL => "sys_poll",
        SYS_MKNOD => "sys_mknod",
        SYS_STAT => "sys_stat",
        _ => "unknown",
    }
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

// ─────────────────────────────────────────────────────────────
//  Public syscall wrappers
// ─────────────────────────────────────────────────────────────

/// sys_mknod — create a named FIFO (special file)
pub fn sc_mkfifo(path: &str) -> (SyscallRecord, Result<(), String>) {
    let cpath = CString::new(path).expect("CString::new failed");
    // mode = S_IFIFO | 0o666
    let mode: mode_t = S_IFIFO | 0o666;
    let result = unsafe { libc::mknod(cpath.as_ptr(), mode, 0) };
    let rec = SyscallRecord::new(
        SYS_MKNOD,
        "sys_mknod",
        vec![
            format!("\"{}\"", path),
            format!("{:#o} (S_IFIFO|0666)", mode),
            "0".to_string(),
        ],
        result as i64,
        now_str(),
    );
    if result == 0 || unsafe { *libc::__errno_location() } == libc::EEXIST {
        (rec, Ok(()))
    } else {
        let err = std::io::Error::last_os_error();
        (rec, Err(format!("mknod failed: {}", err)))
    }
}

/// sys_open — open a file/FIFO and return raw fd
pub fn sc_open(path: &str, flags: c_int) -> (SyscallRecord, Result<c_int, String>) {
    let cpath = CString::new(path).expect("CString::new failed");
    let fd = unsafe { libc::open(cpath.as_ptr(), flags) };
    let rec = SyscallRecord::new(
        SYS_OPEN,
        "sys_open",
        vec![
            format!("\"{}\"", path),
            format!("{:#010x}", flags),
        ],
        fd as i64,
        now_str(),
    );
    if fd >= 0 {
        (rec, Ok(fd))
    } else {
        let err = std::io::Error::last_os_error();
        (rec, Err(format!("open failed: {}", err)))
    }
}

/// sys_write — write bytes to fd
pub fn sc_write(fd: c_int, buf: &[u8]) -> (SyscallRecord, Result<usize, String>) {
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const c_void, buf.len() as size_t) };
    let rec = SyscallRecord::new(
        SYS_WRITE,
        "sys_write",
        vec![
            format!("{}", fd),
            format!("{:#018x}", buf.as_ptr() as usize),
            format!("{}", buf.len()),
        ],
        n as i64,
        now_str(),
    );
    if n >= 0 {
        (rec, Ok(n as usize))
    } else {
        let err = std::io::Error::last_os_error();
        (rec, Err(format!("write failed: {}", err)))
    }
}

/// sys_read — read bytes from fd into buf (non-blocking)
pub fn sc_read(fd: c_int, buf: &mut [u8]) -> (SyscallRecord, Result<usize, String>) {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut c_void, buf.len() as size_t) };
    let rec = SyscallRecord::new(
        SYS_READ,
        "sys_read",
        vec![
            format!("{}", fd),
            format!("{:#018x}", buf.as_ptr() as usize),
            format!("{}", buf.len()),
        ],
        n as i64,
        now_str(),
    );
    if n >= 0 {
        (rec, Ok(n as usize))
    } else {
        let errno = unsafe { *libc::__errno_location() };
        if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
            (rec, Ok(0)) // no data yet
        } else {
            let err = std::io::Error::last_os_error();
            (rec, Err(format!("read failed: {}", err)))
        }
    }
}

/// sys_close — close a file descriptor
#[allow(dead_code)]
pub fn sc_close(fd: c_int) -> SyscallRecord {
    let result = unsafe { libc::close(fd) };
    SyscallRecord::new(
        SYS_CLOSE,
        "sys_close",
        vec![format!("{}", fd)],
        result as i64,
        now_str(),
    )
}

/// sys_poll — check if fd has data ready to read (timeout_ms=0 for non-blocking)
pub fn sc_poll(fd: c_int, timeout_ms: c_int) -> (SyscallRecord, bool) {
    let mut pfd = libc::pollfd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1 as nfds_t, timeout_ms) };
    let ready = result > 0 && (pfd.revents & POLLIN) != 0;
    let rec = SyscallRecord::new(
        SYS_POLL,
        "sys_poll",
        vec![
            format!("&pollfd{{fd:{},events:POLLIN}}", fd),
            "1".to_string(),
            format!("{}", timeout_ms),
        ],
        result as i64,
        now_str(),
    );
    (rec, ready)
}
