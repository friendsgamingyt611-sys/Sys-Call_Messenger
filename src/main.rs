// ============================================================
//  main.rs — SysCall Messenger
//
//  The tool wraps user-supplied syscall arguments into a real
//  C program, compiles it, and runs it. No Rust-level parsing.
//  The kernel call is made by the compiled C binary itself.
//
//  USAGE
//    scm "<syscall args>"              args only — tool wraps in template
//    scm -e "<syscall args>"           open full C file in editor first
//    scm -c <compiler> "<args>"        use a specific compiler
//
//  EXAMPLES
//    scm "SYS_write, 1, \"hello\\n\", 6"
//    scm "SYS_read, 0, buf, sizeof(buf)"
//    scm -e "SYS_write, 1, \"hello\\n\", 6"
//    scm -c /usr/bin/clang "SYS_write, 1, \"hi\\n\", 3"
//
// ============================================================

mod compiler;
mod daemon;
mod editor;
mod messenger;
mod syscall_layer;
mod ui;

fn print_usage() {
    println!("\x1b[1mSysCall Messenger\x1b[0m  v1.0.0");
    println!();
    println!("  Make any Linux syscall or libc function call from the command line.");
    println!("  SCM auto-discovers headers via man pages, builds a real C program in your code space,");
    println!("  compiles it with gcc/clang, runs it, and shows the kernel return value.");
    println!();
    println!("\x1b[93m  code space:\x1b[0m");
    println!("    • In normal mode: code space is unique for every syscall you run.");
    println!("    • In -R (runner session) mode: code space persists and accumulates across calls.");
    println!();
    println!("\x1b[93m  code space & REPL commands:\x1b[0m");
    println!("    \x1b[92mcs\x1b[0m                   instantly print the current C code space (with line numbers)");
    println!("    \x1b[92mcs run\x1b[0m               compile and run the current C code space");
    println!("    \x1b[92mclr\x1b[0m                  clear code space and erase all memory/variables (also clear/reset)");
    println!("    \x1b[92mfc [L<num>|num]\x1b[0m       focus kernel return value evaluation on line L<num> or statement (or list all)");
    println!("    \x1b[92mins L<num> NULL\x1b[0m       insert a blank line at position <num> and shift below lines down");
    println!("    \x1b[92mins L<num> <code...>\x1b[0m  insert a new code line at position <num> and shift below lines down");
    println!("    \x1b[92mrpc L<num> <code...>\x1b[0m  replace code line <num> in code space (e.g. rpc L12 read(fd, buf, 100))");
    println!("    \x1b[92mrpc L<num> NULL\x1b[0m      delete code line <num> from code space (also accepts del/delete)");
    println!("    \x1b[92mrpc L*\x1b[0m               clear all lines from code space (also accepts rpc L *)");
    println!("    \x1b[92msession\x1b[0m              view current code space state and variables");
    println!();
    println!("\x1b[93m  usage:\x1b[0m");
    println!("    scm [flags] '<call>'          single-shot mode");
    println!("    scm [flags]                   interactive REPL");
    println!();
    println!("\x1b[93m  flags:\x1b[0m");
    println!("    \x1b[92m-q\x1b[0m, \x1b[92m--quick\x1b[0m           quick mode: suppress C code preview, run in background and print output instantly");
    println!("    \x1b[92m-R\x1b[0m, \x1b[92m--runner-session\x1b[0m  persistent runner mode (code space, state, FDs, and variables persist across commands)");
    println!("    \x1b[92m-e\x1b[0m, \x1b[92m--editor\x1b[0m          open code space in the built-in editor before compiling");
    println!("                           (Ctrl+S to save+run, Esc to cancel)");
    println!("    \x1b[92m-y\x1b[0m, \x1b[92m--yes\x1b[0m             auto-confirm all prompts (compile, header fixes, memory inspect)");
    println!("    \x1b[92m-m\x1b[0m, \x1b[92m--mem-inspect\x1b[0m     always inspect memory when the kernel returns a pointer-like value");
    println!("                           (auto-detects address range; no prompt needed)");
    println!("    \x1b[92m-c\x1b[0m, \x1b[92m--compiler\x1b[0m <bin>  use a specific C compiler binary  (default: gcc → clang → cc)");
    println!("    \x1b[92m--tui\x1b[0m              launch the split-pane TUI messenger interface");
    println!("    \x1b[92m-h\x1b[0m, \x1b[92m--help\x1b[0m            show this help text");
    println!();
    println!("\x1b[93m  examples:\x1b[0m");
    println!("    scm 'write(1, \"hello\\n\", 6)'");
    println!("    scm 'openat(AT_FDCWD, \"file.txt\", O_RDONLY)'");
    println!("    scm 'socket(AF_INET, SOCK_STREAM, 0)'");
    println!("    scm '(SYS_write, 1, \"hello\\n\", 6)'          raw syscall via syscall(2)");
    println!("    scm -q 'getpid()'                              quick mode (print output instantly without C box)");
    println!("    scm -R                                         start REPL in persistent session mode");
    println!("    scm -e 'getcwd(buf, sizeof(buf))'              edit code space before running");
    println!("    scm -m 'getcwd(buf, sizeof(buf))'              force memory inspection of returned pointer");
    println!("    scm -y 'getpid()'                              skip all prompts");
    println!("    scm -c clang 'write(1, \"hi\\n\", 3)'           use clang instead of gcc");
    println!();
    println!("\x1b[93m  memory inspector (-m):\x1b[0m");
    println!("    When a syscall returns a value in the user-space address range");
    println!("    (0x10000 – 0x7FFFFFFFFFFF), SCM recognises it as a pointer and");
    println!("    offers to inspect the memory:");
    println!("      • /proc/self/maps region  (e.g. [stack], [heap], library name)");
    println!("      • string interpretation   (null-terminated C string check)");
    println!("      • 64-byte hex dump        (with ASCII panel)");
    println!("    The inspector uses setjmp/longjmp to catch SIGSEGV safely.");
    println!();
    println!("\x1b[93m  C template:\x1b[0m");
    println!("    #include <unistd.h>");
    println!("    #include <sys/syscall.h>");
    println!("    /* headers auto-discovered from man pages */");
    println!("    int main(void) {{");
    println!("        long res = (long)(<your call>);");
    println!("        printf(\"[kernel return value: %ld]\\n\", res);");
    println!("        return 0;");
    println!("    }}");
}

fn print_banner(runner_session: bool) {
    println!("\x1b[1m╔══════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[1m║                   SysCall Messenger                  ║\x1b[0m");
    println!("\x1b[1m╚══════════════════════════════════════════════════════╝\x1b[0m");
    if runner_session {
        println!(" \x1b[93m[Persistent Runner Session (-R) Active — FDs, state, & code space persist]\x1b[0m");
        println!(" Type \x1b[93mcs\x1b[0m to view code space. Type \x1b[93mcs run\x1b[0m to execute code space.");
        println!(" Type \x1b[93mins L<num> NULL\x1b[0m to insert blank line, \x1b[93mrpc L<num> <code...>\x1b[0m to edit line, \x1b[93mrpc L<num> NULL\x1b[0m to delete line, or \x1b[93mrpc L*\x1b[0m to erase all lines.");
    }
    println!(" Type any function/syscall (e.g. \x1b[92mopenat(AT_FDCWD, \"file.txt\", O_RDONLY)\x1b[0m or \x1b[92mwrite(1, \"hello\\n\", 6)\x1b[0m)");
    println!(" Type \x1b[93mcs\x1b[0m to view code space. Type \x1b[93mclr\x1b[0m to clear code space. Type \x1b[93mexit\x1b[0m to exit. Type \x1b[93mhelp\x1b[0m for usage.\n");
}

fn clean_prompt_prefixes(raw: &str) -> String {
    let mut clean = raw.trim();
    loop {
        let before = clean;
        if clean.starts_with("scm>") {
            clean = clean[4..].trim();
        } else if clean.starts_with("scm:") {
            clean = clean[4..].trim();
        } else if clean.starts_with("scm ") {
            clean = clean[4..].trim();
        } else if clean.starts_with("scm\t") {
            clean = clean[4..].trim();
        }
        if clean == before {
            break;
        }
    }
    clean.to_string()
}

pub fn process_command(
    input: &str,
    session: &mut compiler::SessionRunner,
    quick_mode: bool,
    editor_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<&str>,
    runner_session: bool,
) {
    let clean_str = clean_prompt_prefixes(input);
    let trimmed = clean_str.trim();
    if trimmed.is_empty() {
        return;
    }

    match trimmed {
        "exit" | "quit" | "q" => {
            println!("\x1b[90mGoodbye!\x1b[0m");
            std::process::exit(0);
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        s if s == "cs" || s == "codespace" || s.starts_with("cs ") || s.starts_with("codespace ") => {
            let rest = if s.starts_with("codespace") {
                s["codespace".len()..].trim()
            } else {
                s["cs".len()..].trim()
            };

            if rest == "run" {
                compiler::run_session_direct(session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path);
            } else if rest.is_empty() {
                compiler::print_codespace(session);
            } else {
                println!("  \x1b[91m✗  unknown cs command: '{}'. Use 'cs' to view code space or 'cs run' to compile and run.\x1b[0m\n", rest);
            }
        }
        "clr" | "clear" | "reset" => {
            session.reset();
            use std::io::Write;
            print!("\x1b[H\x1b[2J\x1b[3J");
            let _ = std::io::stdout().flush();
            print_banner(runner_session);
        }
        "session" | "status" => {
            println!("  \x1b[93mCode Space State:\x1b[0m");
            println!("    Headers:    {:?}", session.extra_headers);
            println!("    Variables:  {:?}", session.extra_vars);
            println!("    Statements:");
            if session.statements.is_empty() {
                println!("      (none)");
            } else {
                for (idx, st) in session.statements.iter().enumerate() {
                    let focus_mark = match session.focus_index {
                        Some(f_idx) if f_idx == idx => " \x1b[92m<-- focused\x1b[0m",
                        None if idx + 1 == session.statements.len() => " \x1b[92m<-- focused (default)\x1b[0m",
                        _ => "",
                    };
                    println!("      \x1b[93mL{}:\x1b[0m {}{}", idx + 1, st, focus_mark);
                }
            }
            println!();
        }
        s if s.to_lowercase().starts_with("fc") || s.to_lowercase().starts_with("focus") => {
            if session.statements.is_empty() {
                println!("  \x1b[91m✗  code space has no active statements/syscalls to focus on. Add statements first.\x1b[0m\n");
                return;
            }

            let rest = if s.to_lowercase().starts_with("focus") {
                s[5..].trim()
            } else {
                s[2..].trim()
            };

            if rest.is_empty() {
                let lines_info = compiler::get_statement_lines(session);
                println!("  \x1b[93mCode Space Syscalls / Statements:\x1b[0m");
                for (idx, start, end, stmt, is_focused) in &lines_info {
                    if stmt.is_empty() || stmt == ";" {
                        println!("    \x1b[90m[{}] (C Line L{}): (blank line / space)\x1b[0m", idx + 1, start);
                    } else {
                        let focus_mark = if *is_focused { " \x1b[92m<-- current focus\x1b[0m" } else { "" };
                        if start == end {
                            println!("    \x1b[93m[{}]\x1b[0m (C Line L{}): {}{}", idx + 1, start, stmt, focus_mark);
                        } else {
                            println!("    \x1b[93m[{}]\x1b[0m (C Lines L{}-L{}): {}{}", idx + 1, start, end, stmt, focus_mark);
                        }
                    }
                }
                let printed_count = lines_info.len();
                if printed_count < session.statements.len() {
                    for idx in printed_count..session.statements.len() {
                        let stmt = session.statements[idx].trim_end_matches(';').trim();
                        println!("    \x1b[90m[{}] (below focus — omitted from execution): {}\x1b[0m", idx + 1, stmt);
                    }
                }
                println!();
                print!("  \x1b[93mselect line or statement number to focus on [1..{} or Ln]: \x1b[0m", session.statements.len());
                use std::io::Write;
                std::io::stdout().flush().unwrap();

                let mut choice = String::new();
                if std::io::stdin().read_line(&mut choice).is_ok() {
                    let choice_trim = choice.trim();
                    if choice_trim.is_empty() {
                        return;
                    }
                    match compiler::parse_focus_input(choice_trim, session) {
                        Ok(target_opt) => {
                            let old_session = session.clone();
                            session.focus_index = target_opt;
                            if let Some(idx) = target_opt {
                                println!("  \x1b[92m✓  focused kernel return value on statement [{}] (C line L{}): {}\x1b[0m", idx + 1, lines_info[idx].1, lines_info[idx].3);
                            } else {
                                println!("  \x1b[92m✓  reset focus to latest statement\x1b[0m");
                            }
                            compiler::run_session_rerun_rpc(session, old_session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path);
                            println!();
                        }
                        Err(e) => {
                            println!("  \x1b[91m✗  {}\x1b[0m\n", e);
                        }
                    }
                }
            } else {
                let lines_info = compiler::get_statement_lines(session);
                match compiler::parse_focus_input(rest, session) {
                    Ok(target_opt) => {
                        let old_session = session.clone();
                        session.focus_index = target_opt;
                        if let Some(idx) = target_opt {
                            let start_line = if idx < lines_info.len() { lines_info[idx].1 } else { 0 };
                            let stmt_str = if idx < lines_info.len() { &lines_info[idx].3 } else { "" };
                            println!("  \x1b[92m✓  focused kernel return value on statement [{}] (C line L{}): {}\x1b[0m", idx + 1, start_line, stmt_str);
                        } else {
                            println!("  \x1b[92m✓  reset focus to latest statement\x1b[0m");
                        }
                        compiler::run_session_rerun_rpc(session, old_session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path);
                        println!();
                    }
                    Err(e) => {
                        println!("  \x1b[91m✗  {}\x1b[0m\n", e);
                    }
                }
            }
        }
        s if s.to_lowercase().starts_with("ins") || s.to_lowercase().starts_with("insert") => {
            let cmd_prefix = if s.to_lowercase().starts_with("insert") { "insert" } else { "ins" };
            if let Some(insertions) = compiler::parse_line_command(s, cmd_prefix) {
                if insertions.is_empty() {
                    println!("  \x1b[91m✗  invalid ins syntax: line target missing (e.g. use 'ins L11 NULL' to insert a blank line or 'ins L11 <code...>')\x1b[0m");
                    println!("  \x1b[93mUsage:\x1b[0m   ins L11 NULL (to insert blank line)  or  ins L11 <code...> (to insert line and shift down below lines)\n");
                    return;
                }

                let old_session = session.clone();

                for (num, code) in insertions {
                    let code_trim = code.trim();
                    let code_lower = code_trim.to_lowercase();
                    let is_blank_insert = code_trim.is_empty() || matches!(code_lower.as_str(), "null" | "none" | "blank" | "space" | "newline" | "/n" | "\\n" | "/n," | "/n ,,");
                    let target = compiler::resolve_c_line_target(num, session);

                    match target {
                        compiler::LineTarget::Statement(idx) | compiler::LineTarget::InsertAfter(idx) => {
                            let pos = idx.min(session.statements.len());
                            if is_blank_insert {
                                session.statements.insert(pos, "".to_string());
                                println!("  \x1b[92m✓  inserted blank line space at C line L{}, shifted below lines down\x1b[0m", num);
                            } else {
                                let prepared = match compiler::prepare_c_statement(&code) {
                                    Ok(p) => p,
                                    Err(_) => format!("{};", code_trim.trim_end_matches(';')),
                                };
                                session.statements.insert(pos, prepared.clone());
                                println!("  \x1b[92m✓  inserted statement at C line L{}: {}, shifted below lines down\x1b[0m", num, prepared);
                            }
                        }
                        compiler::LineTarget::ExtraVar(idx) => {
                            let pos = idx.min(session.extra_vars.len());
                            if is_blank_insert {
                                session.extra_vars.insert(pos, "//".to_string());
                                println!("  \x1b[92m✓  inserted blank line space at variable C line L{}, shifted below lines down\x1b[0m", num);
                            } else {
                                let prepared = if code_trim.ends_with(';') { code_trim.to_string() } else { format!("{};", code_trim) };
                                session.extra_vars.insert(pos, prepared.clone());
                                println!("  \x1b[92m✓  inserted variable at C line L{}: {}, shifted below lines down\x1b[0m", num, prepared);
                            }
                        }
                        compiler::LineTarget::Header(idx) => {
                            let pos = idx.min(session.extra_headers.len());
                            let header = code_trim.trim_start_matches('#').trim_start_matches("include").trim().trim_matches(&['<', '>', '"'][..]);
                            if !header.is_empty() && !session.extra_headers.contains(&header.to_string()) {
                                session.extra_headers.insert(pos, header.to_string());
                                println!("  \x1b[92m✓  inserted header <{}> at C line L{}, shifted below lines down\x1b[0m", header, num);
                            }
                        }
                    }
                }

                if !session.statements.is_empty() || !session.extra_vars.is_empty() {
                    compiler::run_session_rerun_rpc(session, old_session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path);
                    println!();
                } else {
                    println!("  \x1b[93mSession code space cleared.\x1b[0m\n");
                }
            } else {
                let sess_opt = if runner_session { Some(session) } else { None };
                compiler::run(trimmed, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, sess_opt);
                println!();
            }
        }
        s if s.to_lowercase().starts_with("rpc") => {
            if let Some(replacements) = compiler::parse_rpc_command(s) {
                if replacements.is_empty() {
                    println!("  \x1b[91m✗  invalid rpc syntax: line target missing (e.g. use 'rpc L1 <code...>' to replace line 1 or 'rpc L*' to clear)\x1b[0m");
                    println!("  \x1b[93mUsage:\x1b[0m   rpc L1 <codeline> [L2 <codeline> ...]  or  rpc L1 NULL (to delete)  or  rpc L* (to clear all)\n");
                    return;
                }
                if replacements.iter().any(|(num, _)| *num == usize::MAX) {
                    session.statements.clear();
                    session.extra_vars.clear();
                    println!("  \x1b[92m✓  cleared code space (all lines erased)\x1b[0m\n");
                    return;
                }

                let old_session = session.clone();
                let mut stmts_to_remove: Vec<usize> = Vec::new();
                let mut vars_to_remove: Vec<usize> = Vec::new();

                for (num, code) in replacements {
                    let code_trim = code.trim();
                    let code_lower = code_trim.to_lowercase();
                    let is_delete = code_trim.is_empty() || matches!(code_lower.as_str(), "null" | "del" | "delete" | "remove" | "drop" | "none");
                    let is_insert_newline = matches!(code_lower.as_str(), "/n" | "\\n" | "/n," | "/n ,," | "newline" | "space" | "blank" | "insert");
                    let target = compiler::resolve_c_line_target(num, session);

                    match target {
                        compiler::LineTarget::Statement(idx) => {
                            if is_delete {
                                if !stmts_to_remove.contains(&idx) {
                                    stmts_to_remove.push(idx);
                                }
                            } else if is_insert_newline {
                                session.statements.insert(idx, "".to_string());
                                println!("  \x1b[92m✓  inserted blank line space at C line L{}, shifted below lines down\x1b[0m", num);
                            } else {
                                let prepared = match compiler::prepare_c_statement(&code) {
                                    Ok(p) => p,
                                    Err(_) => format!("{};", code_trim.trim_end_matches(';')),
                                };
                                session.statements[idx] = prepared;
                                println!("  \x1b[92m✓  updated statement (C line L{}): {}\x1b[0m", num, session.statements[idx]);
                            }
                        }
                        compiler::LineTarget::InsertAfter(idx) => {
                            let pos = idx.min(session.statements.len());
                            if is_delete {
                                if pos > 0 && pos - 1 < session.statements.len() {
                                    if !stmts_to_remove.contains(&(pos - 1)) {
                                        stmts_to_remove.push(pos - 1);
                                    }
                                }
                            } else if is_insert_newline {
                                session.statements.insert(pos, "".to_string());
                                println!("  \x1b[92m✓  inserted blank line space at C line L{}, shifted below lines down\x1b[0m", num);
                            } else {
                                let prepared = match compiler::prepare_c_statement(&code) {
                                    Ok(p) => p,
                                    Err(_) => format!("{};", code_trim.trim_end_matches(';')),
                                };
                                if pos < session.statements.len() {
                                    session.statements[pos] = prepared;
                                    println!("  \x1b[92m✓  updated statement (C line L{}): {}\x1b[0m", num, session.statements[pos]);
                                } else {
                                    session.statements.push(prepared);
                                    println!("  \x1b[92m✓  added statement at C line L{}: {}\x1b[0m", num, session.statements.last().unwrap());
                                }
                            }
                        }
                        compiler::LineTarget::ExtraVar(idx) => {
                            if is_delete {
                                if !vars_to_remove.contains(&idx) {
                                    vars_to_remove.push(idx);
                                }
                            } else if is_insert_newline {
                                session.extra_vars.insert(idx, "//".to_string());
                                println!("  \x1b[92m✓  inserted blank line space at variable C line L{}, shifted below lines down\x1b[0m", num);
                            } else {
                                let prepared = if code_trim.ends_with(';') { code_trim.to_string() } else { format!("{};", code_trim) };
                                session.extra_vars[idx] = prepared;
                                println!("  \x1b[92m✓  updated variable (C line L{}): {}\x1b[0m", num, session.extra_vars[idx]);
                            }
                        }
                        compiler::LineTarget::Header(idx) => {
                            if is_delete && idx < session.extra_headers.len() {
                                session.extra_headers.remove(idx);
                                println!("  \x1b[92m✓  removed header at C line L{}\x1b[0m", num);
                            }
                        }
                    }
                }

                stmts_to_remove.sort_unstable_by(|a, b| b.cmp(a));
                for idx in stmts_to_remove {
                    if idx < session.statements.len() {
                        let removed = session.statements.remove(idx);
                        println!("  \x1b[92m✓  removed statement: {}\x1b[0m", removed);
                    }
                }

                vars_to_remove.sort_unstable_by(|a, b| b.cmp(a));
                for idx in vars_to_remove {
                    if idx < session.extra_vars.len() {
                        let removed = session.extra_vars.remove(idx);
                        println!("  \x1b[92m✓  removed variable: {}\x1b[0m", removed);
                    }
                }

                if !session.statements.is_empty() || !session.extra_vars.is_empty() {
                    compiler::run_session_rerun_rpc(session, old_session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path);
                    println!();
                } else {
                    println!("  \x1b[93mSession code space cleared.\x1b[0m\n");
                }
            } else {
                let sess_opt = if runner_session { Some(session) } else { None };
                compiler::run(trimmed, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, sess_opt);
                println!();
            }
        }
        _ => {
            let sess_opt = if runner_session { Some(session) } else { None };
            compiler::run(trimmed, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, sess_opt);
            println!();
        }
    }
}

fn run_repl(quick_mode: bool, editor_mode: bool, auto_confirm: bool, mem_inspect: bool, compiler_path: Option<&str>, runner_session: bool) {
    use std::io::{self, Write};
    use std::time::{Duration, Instant};

    print_banner(runner_session);

    let mut session = compiler::SessionRunner::new();
    let mut last_command = String::new();
    let mut last_time = Instant::now();

    loop {
        print!("\x1b[96mscm>\x1b[0m ");
        if io::stdout().flush().is_err() { break; }

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) | Err(_) => {
                println!();
                break;
            }
            Ok(_) => {}
        }

        let cleaned = clean_prompt_prefixes(&input);
        if cleaned.is_empty() {
            continue;
        }

        let now = Instant::now();
        // Debounce identical scroll-click / middle-click paste within 250ms
        if cleaned == last_command && now.duration_since(last_time) < Duration::from_millis(250) {
            continue;
        }

        last_command = cleaned.clone();
        last_time = now;

        process_command(&cleaned, &mut session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, runner_session);
    }
}


fn main() {
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.len() >= 3 && raw_args[1] == "--daemon-worker" {
        daemon::run_daemon_worker(&raw_args[2]);
        return;
    }

    // ── Parse our own flags ──────────────────────────────────
    let mut quick_mode    = false;
    let mut editor_mode    = false;
    let mut auto_confirm   = false;
    let mut mem_inspect    = false;
    let mut runner_session = false;
    let mut compiler: Option<String> = None;
    let mut commands: Vec<String> = Vec::new();
    let mut tui_mode = false;

    let mut i = 1;
    while i < raw_args.len() {
        match raw_args[i].as_str() {
            "-h" | "--help" => {
                print_usage();
                return;
            }
            "--tui" => { tui_mode = true; }
            "-q" | "--quick" => { quick_mode = true; }
            "-R" | "--runner" | "--runner-session" => { runner_session = true; }
            "-e" | "--editor"       => { editor_mode  = true; }
            "-y" | "--yes"          => { auto_confirm = true; }
            "-m" | "--mem-inspect"  => { mem_inspect  = true; }
            "-c" | "--compiler" => {
                i += 1;
                if i < raw_args.len() {
                    compiler = Some(raw_args[i].clone());
                } else {
                    eprintln!("\x1b[91merror:\x1b[0m -c requires a compiler path");
                    std::process::exit(1);
                }
            }
            arg => {
                commands.push(arg.to_string());
            }
        }
        i += 1;
    }

    // ── TUI mode ─────────────────────────────────────────────
    if tui_mode {
        ui::run_tui(
            runner_session,
            quick_mode,
            auto_confirm,
            mem_inspect,
            compiler.clone(),
        );
        return;
    }

    // ── Single/Multi CLI command vs Interactive REPL ──────────
    if commands.is_empty() {
        run_repl(quick_mode, editor_mode, auto_confirm, mem_inspect, compiler.as_deref(), runner_session);
    } else {
        let mut session = compiler::SessionRunner::new();
        for cmd in &commands {
            process_command(cmd, &mut session, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler.as_deref(), runner_session);
        }
    }
}

