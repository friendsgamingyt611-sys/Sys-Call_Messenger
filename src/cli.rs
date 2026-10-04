// ============================================================
//  cli.rs — Syscall expression executor
//
//  Accepts:  sys_write(fifo, message)
//            sys_read(fifo)
//            sys_listen(fifo)
//            sys_open(fifo)
//            sys_close(<fd>)
//            sys_mknod(/tmp/path)
//            sys_poll(fifo)...upto all ~450+ linux syscalls(apparently)
//
//  Anything else → error. "hello" is not a syscall.
// ============================================================

use std::io;
use std::thread;
use std::time::Duration;

use libc::O_NONBLOCK;

use crate::syscall_layer::{sc_close, sc_mkfifo, sc_open, sc_poll, sc_read, sc_write};

const DEFAULT_FIFO: &str = "/tmp/syscall_messenger.fifo";

// ── ANSI colours ─────────────────────────────────────────────
const RESET:   &str = "\x1b[0m";
const BOLD:    &str = "\x1b[1m";
const DIM:     &str = "\x1b[2m";
const CYAN:    &str = "\x1b[96m";
const GREEN:   &str = "\x1b[92m";
const RED:     &str = "\x1b[91m";
const MAGENTA: &str = "\x1b[95m";

fn show(name: &str, nr: u64, result: i64, asm: &str) {
    let col = if result >= 0 { GREEN } else { RED };
    println!("  {DIM}[syscall #{nr}]{RESET} {CYAN}{name}{RESET} → {col}{result}{RESET}",
        DIM=DIM, nr=nr, RESET=RESET, CYAN=CYAN, name=name, col=col);
    for line in asm.lines() {
        println!("    {DIM}{line}{RESET}", DIM=DIM, line=line, RESET=RESET);
    }
}

/// Resolve "fifo" shorthand to the default FIFO path
fn resolve(r: &str) -> &str {
    if r == "fifo" || r.is_empty() { DEFAULT_FIFO } else { r }
}

// ─────────────────────────────────────────────────────────────
//  Entry point — parse sys_name(args) and dispatch
// ─────────────────────────────────────────────────────────────
pub fn execute(expr: &str) {
    let expr = expr.trim();

    // Split at '(' to get: ("sys_write", "fifo, hello world)")
    let paren = match expr.find('(') {
        Some(i) => i,
        None => {
            eprintln!("{RED}error:{RESET} missing '(' in \"{}\"\n  e.g. sys_write(fifo, hello world)", expr, RED=RED, RESET=RESET);
            std::process::exit(1);
        }
    };

    let syscall_name = expr[..paren].trim().to_lowercase();
    let inner = expr[paren + 1..].trim_end_matches(')').trim();

    // Split args: first comma separates resource from the rest (message may contain commas)
    let (resource_raw, rest) = match inner.find(',') {
        Some(i) => (inner[..i].trim(), inner[i + 1..].trim()),
        None    => (inner.trim(), ""),
    };

    let path = resolve(resource_raw);

    println!("{BOLD}{}{RESET}", expr, BOLD=BOLD, RESET=RESET);
    println!();

    match syscall_name.as_str() {
        "sys_write" => do_write(path, rest),
        "sys_read"  => do_read(path),
        "sys_listen"=> do_listen(path),
        "sys_open"  => do_open(path),
        "sys_close" => {
            match resource_raw.parse::<i32>() {
                Ok(fd) => do_close(fd),
                Err(_) => {
                    eprintln!("{RED}error:{RESET} sys_close needs an fd number, e.g. sys_close(9)", RED=RED, RESET=RESET);
                    std::process::exit(1);
                }
            }
        }
        "sys_mknod" => do_mknod(path),
        "sys_poll"  => do_poll(path),
        unknown => {
            eprintln!("{RED}error:{RESET} unknown syscall: {BOLD}{}{RESET}", unknown, RED=RED, RESET=RESET, BOLD=BOLD);
            eprintln!("  valid syscalls: sys_write, sys_read, sys_listen, sys_open, sys_close, sys_mknod, sys_poll");
            std::process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  sys_write(fifo, message)
// ─────────────────────────────────────────────────────────────
fn do_write(path: &str, message: &str) {
    if message.is_empty() {
        eprintln!("{RED}error:{RESET} sys_write requires a message argument", RED=RED, RESET=RESET);
        eprintln!("  e.g.  sys_write(fifo, hello world)");
        std::process::exit(1);
    }

    // Ensure FIFO exists
    let (rec, _) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);

    // O_WRONLY — requires a reader already open
    let (rec_o, fd_res) = sc_open(path, libc::O_WRONLY | O_NONBLOCK);
    show(rec_o.name, rec_o.number, rec_o.result, &rec_o.asm_snippet);

    let fd = match fd_res {
        Ok(fd) => fd,
        Err(ref e) if e.contains("os error 6") || e.contains("No such device") => {
            eprintln!("\n  {RED}✗  ENXIO — no reader open on the other end{RESET}", RED=RED, RESET=RESET);
            eprintln!("  {DIM}Start a listener in another terminal first:{RESET}", DIM=DIM, RESET=RESET);
            eprintln!("    scm \"sys_listen(fifo)\"");
            std::process::exit(1);
        }
        Err(e) => { eprintln!("  {RED}error:{RESET} {}", e, RED=RED, RESET=RESET); std::process::exit(1); }
    };

    let packet = format!("{}\n", message);
    let (rec_w, res_w) = sc_write(fd, packet.as_bytes());
    show(rec_w.name, rec_w.number, rec_w.result, &rec_w.asm_snippet);

    match res_w {
        Ok(n) => println!("\n  {GREEN}✓{RESET}  {} bytes in kernel FIFO buffer", n, GREEN=GREEN, RESET=RESET),
        Err(e) => eprintln!("  {RED}✗{RESET}  {}", e, RED=RED, RESET=RESET),
    }

    let (rec_c, _) = (sc_close(fd), ());
    show(rec_c.name, rec_c.number, rec_c.result, &rec_c.asm_snippet);
    println!();
}

// ─────────────────────────────────────────────────────────────
//  sys_read(fifo)
// ─────────────────────────────────────────────────────────────
fn do_read(path: &str) {
    let (rec, _) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);

    let (rec_o, fd_res) = sc_open(path, libc::O_RDWR | O_NONBLOCK);
    show(rec_o.name, rec_o.number, rec_o.result, &rec_o.asm_snippet);

    let fd = match fd_res {
        Ok(fd) => fd,
        Err(e) => { eprintln!("  {RED}error:{RESET} {}", e, RED=RED, RESET=RESET); std::process::exit(1); }
    };

    let (_, ready) = sc_poll(fd, 200);
    if !ready {
        println!("\n  {DIM}(no data in kernel buffer){RESET}", DIM=DIM, RESET=RESET);
        sc_close(fd);
        println!();
        return;
    }

    let mut buf = [0u8; 4096];
    let (rec_r, res_r) = sc_read(fd, &mut buf);
    show(rec_r.name, rec_r.number, rec_r.result, &rec_r.asm_snippet);

    match res_r {
        Ok(n) if n > 0 => {
            println!();
            for line in String::from_utf8_lossy(&buf[..n]).lines() {
                if !line.is_empty() {
                    println!("  {MAGENTA}▶{RESET}  {}", line, MAGENTA=MAGENTA, RESET=RESET);
                }
            }
            println!("\n  {GREEN}✓{RESET}  {} bytes read", n, GREEN=GREEN, RESET=RESET);
        }
        Ok(_) => println!("\n  {DIM}(empty read){RESET}", DIM=DIM, RESET=RESET),
        Err(e) => eprintln!("  {RED}error:{RESET} {}", e, RED=RED, RESET=RESET),
    }

    let rec_c = sc_close(fd);
    show(rec_c.name, rec_c.number, rec_c.result, &rec_c.asm_snippet);
    println!();
}

// ─────────────────────────────────────────────────────────────
//  sys_listen(fifo)  — loops sys_read until Ctrl+C
// ─────────────────────────────────────────────────────────────
fn do_listen(path: &str) {
    println!("  {DIM}Ctrl+C to stop{RESET}\n", DIM=DIM, RESET=RESET);

    let (rec, _) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);

    let (rec_o, fd_res) = sc_open(path, libc::O_RDWR | O_NONBLOCK);
    show(rec_o.name, rec_o.number, rec_o.result, &rec_o.asm_snippet);

    let fd = match fd_res {
        Ok(fd) => fd,
        Err(e) => { eprintln!("  {RED}error:{RESET} {}", e, RED=RED, RESET=RESET); std::process::exit(1); }
    };

    println!("\n  {DIM}waiting on {} …{RESET}\n", path, DIM=DIM, RESET=RESET);

    let mut buf = [0u8; 4096];
    loop {
        let (_, ready) = sc_poll(fd, 300);
        if ready {
            let (rec_r, res_r) = sc_read(fd, &mut buf);
            show(rec_r.name, rec_r.number, rec_r.result, &rec_r.asm_snippet);
            if let Ok(n) = res_r {
                if n > 0 {
                    for line in String::from_utf8_lossy(&buf[..n]).lines() {
                        if !line.is_empty() {
                            println!("  {MAGENTA}▶{RESET}  {}", line, MAGENTA=MAGENTA, RESET=RESET);
                        }
                    }
                    println!();
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// ─────────────────────────────────────────────────────────────
//  sys_open(fifo)
// ─────────────────────────────────────────────────────────────
fn do_open(path: &str) {
    let (rec, _) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);

    let (rec_o, fd_res) = sc_open(path, libc::O_RDWR | O_NONBLOCK);
    show(rec_o.name, rec_o.number, rec_o.result, &rec_o.asm_snippet);

    match fd_res {
        Ok(fd) => println!("\n  {GREEN}✓{RESET}  fd = {CYAN}{}{RESET}", fd, GREEN=GREEN, RESET=RESET, CYAN=CYAN),
        Err(e) => eprintln!("  {RED}✗{RESET}  {}", e, RED=RED, RESET=RESET),
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  sys_close(<fd>)
// ─────────────────────────────────────────────────────────────
fn do_close(fd: i32) {
    let rec = sc_close(fd);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);
    if rec.result == 0 {
        println!("\n  {GREEN}✓{RESET}  fd {} closed", fd, GREEN=GREEN, RESET=RESET);
    } else {
        eprintln!("\n  {RED}✗{RESET}  close failed (invalid fd?)", RED=RED, RESET=RESET);
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  sys_mknod(/tmp/path)
// ─────────────────────────────────────────────────────────────
fn do_mknod(path: &str) {
    let (rec, res) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);
    match res {
        Ok(_) => println!("\n  {GREEN}✓{RESET}  FIFO created: {}", path, GREEN=GREEN, RESET=RESET),
        Err(e) if e.contains("File exists") =>
            println!("\n  {DIM}already exists: {}{RESET}", path, DIM=DIM, RESET=RESET),
        Err(e) => eprintln!("\n  {RED}✗{RESET}  {}", e, RED=RED, RESET=RESET),
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  sys_poll(fifo)
// ─────────────────────────────────────────────────────────────
fn do_poll(path: &str) {
    let (rec, _) = sc_mkfifo(path);
    show(rec.name, rec.number, rec.result, &rec.asm_snippet);

    let (rec_o, fd_res) = sc_open(path, libc::O_RDWR | O_NONBLOCK);
    show(rec_o.name, rec_o.number, rec_o.result, &rec_o.asm_snippet);

    let fd = match fd_res {
        Ok(fd) => fd,
        Err(e) => { eprintln!("  {RED}error:{RESET} {}", e, RED=RED, RESET=RESET); std::process::exit(1); }
    };

    let (rec_p, ready) = sc_poll(fd, 0);
    show(rec_p.name, rec_p.number, rec_p.result, &rec_p.asm_snippet);

    if ready {
        println!("\n  {GREEN}✓{RESET}  POLLIN — data ready to sys_read", GREEN=GREEN, RESET=RESET);
    } else {
        println!("\n  {DIM}  no data in buffer{RESET}", DIM=DIM, RESET=RESET);
    }

    let rec_c = sc_close(fd);
    show(rec_c.name, rec_c.number, rec_c.result, &rec_c.asm_snippet);
    println!();
}
