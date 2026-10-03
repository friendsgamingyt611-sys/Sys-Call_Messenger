// ============================================================
//  daemon.rs — Persistent SCM Daemon Process
//  Enables Live Single-PID Execution across interactive commands
// ============================================================

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub struct DaemonManager {
    pub pid: u32,
    pub socket_path: PathBuf,
    pub child: Child,
}

impl DaemonManager {
    pub fn spawn() -> Result<Self, String> {
        let tmp = std::env::temp_dir();
        let current_pid = std::process::id();
        let socket_path = tmp.join(format!("scm_daemon_{}_{}.sock", current_pid, rand_num()));

        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(&current_exe)
            .args(["--daemon-worker", socket_path.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("failed to spawn daemon process: {}", e))?;

        let daemon_pid = child.id();

        // Wait for daemon worker socket to be bound
        let start = std::time::Instant::now();
        while !socket_path.exists() {
            if start.elapsed() > std::time::Duration::from_secs(3) {
                let _ = child.kill();
                return Err("timed out waiting for persistent daemon worker socket".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        Ok(Self {
            pid: daemon_pid,
            socket_path,
            child,
        })
    }

    pub fn execute_snippet(&self, so_path: &Path) -> Result<i64, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("failed to connect to persistent daemon: {}", e))?;

        let msg = format!("EXEC:{}\n", so_path.display());
        stream.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;

        let mut resp = String::new();
        stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;

        let trimmed = resp.trim();
        if let Some(err) = trimmed.strip_prefix("ERR:") {
            Err(err.to_string())
        } else if let Some(val_str) = trimmed.strip_prefix("OK:") {
            val_str.parse::<i64>().map_err(|e| e.to_string())
        } else {
            Ok(0)
        }
    }

    pub fn shutdown(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let _ = stream.write_all(b"QUIT\n");
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.socket_path.exists() {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

impl Drop for DaemonManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn rand_num() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(12345);
    nanos
}

/// Daemon worker entry point when run as `scm --daemon-worker <socket_path>`
pub fn run_daemon_worker(socket_path: &str) {
    let path = Path::new(socket_path);
    if path.exists() {
        let _ = fs::remove_file(path);
    }

    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Daemon listener bind error: {}", e);
            return;
        }
    };

    type ScmFn = unsafe extern "C" fn() -> i64;

    for stream_res in listener.incoming() {
        let mut stream = match stream_res {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut req = String::new();
        let mut buf = [0u8; 2048];
        if let Ok(n) = stream.read(&mut buf) {
            req = String::from_utf8_lossy(&buf[..n]).to_string();
        }

        let trimmed = req.trim();
        if trimmed == "QUIT" {
            let _ = stream.write_all(b"OK:QUIT\n");
            break;
        }

        if let Some(so_path) = trimmed.strip_prefix("EXEC:") {
            let c_str = match std::ffi::CString::new(so_path) {
                Ok(c) => c,
                Err(e) => {
                    let _ = stream.write_all(format!("ERR:{}\n", e).as_bytes());
                    continue;
                }
            };

            unsafe {
                let handle = libc::dlopen(c_str.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
                if handle.is_null() {
                    let err_ptr = libc::dlerror();
                    let err_msg = if !err_ptr.is_null() {
                        std::ffi::CStr::from_ptr(err_ptr).to_string_lossy().to_string()
                    } else {
                        "unknown dlopen error".to_string()
                    };
                    let _ = stream.write_all(format!("ERR:dlopen failed: {}\n", err_msg).as_bytes());
                    continue;
                }

                let symbol_name = std::ffi::CString::new("scm_exec").unwrap();
                let sym = libc::dlsym(handle, symbol_name.as_ptr());
                if sym.is_null() {
                    let _ = stream.write_all(b"ERR:symbol scm_exec not found\n");
                    let _ = libc::dlclose(handle);
                    continue;
                }

                let scm_fn: ScmFn = std::mem::transmute(sym);
                let res = scm_fn();

                let _ = stream.write_all(format!("OK:{}\n", res).as_bytes());
            }
        }
    }

    if path.exists() {
        let _ = fs::remove_file(path);
    }
}
