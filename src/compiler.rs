// ============================================================
//  compiler.rs — C template builder, editor, compiler, runner
//
//  Flow:
//    1. Build C source from template + user args
//    2. Show generated code to user
//    3. If -e: open in $EDITOR so user can edit freely
//    4. Ask user to confirm before compiling
//    5. Find a C compiler (gcc/clang or user-specified)
//    6. Compile to a temp binary
//    7. Run the binary, print output
//    8. Clean up temp files
// ============================================================

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

// ── ANSI colours ─────────────────────────────────────────────
const RESET:  &str = "\x1b[0m";
const BOLD:   &str = "\x1b[1m";
const DIM:    &str = "\x1b[2m";
const CYAN:   &str = "\x1b[96m";
const GREEN:  &str = "\x1b[92m";
const YELLOW: &str = "\x1b[93m";
const RED:    &str = "\x1b[91m";
const BLUE:   &str = "\x1b[94m";

/// Prepare C statement from user input.
/// Accepts:
///   - Direct function calls: openat(AT_FDCWD, "file.txt", O_RDONLY)
///   - Direct syscall calls:  syscall(SYS_write, 1, "hi\n", 3)
///   - Parenthesised args:    (SYS_write, 1, "hi\n", 3)
fn is_valid_c_declaration(s: &str) -> bool {
    let first_word = s.split_whitespace().next().unwrap_or("");
    matches!(
        first_word,
        "char" | "int" | "long" | "short" | "float" | "double" | "struct" | "union" |
        "enum" | "void" | "size_t" | "ssize_t" | "uint8_t" | "uint16_t" | "uint32_t" |
        "uint64_t" | "int8_t" | "int16_t" | "int32_t" | "int64_t" | "off_t" | "mode_t" |
        "pid_t" | "uid_t" | "gid_t" | "const" | "static" | "unsigned" | "signed"
    )
}

pub fn prepare_c_statement(raw: &str) -> Result<String, String> {
    let mut s = raw.trim();

    if s.is_empty() {
        return Err("input cannot be empty".to_string());
    }

    // Strip outer angle brackets if present: <(SYS_write...)> -> (SYS_write...)
    if s.starts_with('<') && s.ends_with('>') && s.len() >= 2 {
        s = s[1..s.len() - 1].trim();
    }

    // If it starts with '(', it's raw syscall args like (SYS_write, 1, "hello\n", 6)
    if s.starts_with('(') {
        let last_paren = s.rfind(')');
        match last_paren {
            Some(end) if end > 0 => {
                let inner = s[1..end].trim();
                if inner.is_empty() {
                    Err("syscall arguments inside (...) cannot be empty".to_string())
                } else {
                    Ok(format!("syscall({});", inner))
                }
            }
            _ => Err("invalid parenthesised syscall expression".to_string()),
        }
    } else if s.contains('(') && (s.ends_with(')') || s.ends_with(");")) {
        let stmt = s.trim_end_matches(';').trim();
        Ok(format!("{};", stmt))
    } else if s.ends_with(';') || s.contains('=') || is_valid_c_declaration(s) {
        let stmt = s.trim_end_matches(';').trim();
        Ok(format!("{};", stmt))
    } else {
        Err("invalid syntax: provide a function call like openat(...) or syscall args in parentheses like (SYS_write, 1, \"hello\\n\", 6)".to_string())
    }
}

/// Extract target function name from user input
fn extract_function_name(raw: &str) -> Option<String> {
    let mut s = raw.trim();

    if s.starts_with('<') && s.ends_with('>') && s.len() >= 2 {
        s = s[1..s.len() - 1].trim();
    }

    if let Some(paren_pos) = s.find('(') {
        let before_paren = s[..paren_pos].trim();
        let last_token = before_paren.split_whitespace().last().unwrap_or("").trim();
        let func_name = last_token.trim_start_matches('*').trim();
        if !func_name.is_empty() && func_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            if func_name == "syscall" {
                let inner = &s[paren_pos + 1..];
                let first_arg = inner.split(',').next().unwrap_or("").trim();
                return Some(first_arg.to_string());
            }
            return Some(func_name.to_string());
        }
    }

    None
}

/// Query Linux man pages (e.g. man 2 openat) to find required #include <...> header files
fn lookup_headers_from_man(func_name: &str) -> Vec<String> {
    let mut headers = Vec::new();
    let clean_name = func_name
        .trim()
        .trim_start_matches("SYS_")
        .trim_start_matches("sys_")
        .to_lowercase();

    if clean_name.is_empty() {
        return headers;
    }

    for section in &["2", "3", "1", ""] {
        let mut cmd = Command::new("man");
        if !section.is_empty() {
            cmd.arg(section);
        }
        cmd.arg(&clean_name);
        cmd.env("PAGER", "cat");

        if let Ok(out) = cmd.output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut in_synopsis = false;

                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("SYNOPSIS") {
                        in_synopsis = true;
                        continue;
                    }
                    if in_synopsis && (trimmed.starts_with("DESCRIPTION") || trimmed.starts_with("RETURN VALUE") || trimmed.starts_with("LIBRARY")) {
                        break;
                    }
                    if in_synopsis {
                        if let Some(pos) = trimmed.find("#include <").or_else(|| trimmed.find("#include \"")) {
                            let rest = &trimmed[pos + 10..];
                            if let Some(end) = rest.find('>').or_else(|| rest.find('"')) {
                                let header = rest[..end].trim();
                                if !header.is_empty() && !headers.contains(&header.to_string()) {
                                    headers.push(header.to_string());
                                }
                            }
                        }
                    }
                }

                if !headers.is_empty() {
                    break;
                }
            }
        }
    }

    headers
}

/// Detect missing header files from compiler error output notes
fn detect_missing_headers(stderr: &str, included_headers: &[String]) -> Vec<String> {
    let mut detected = Vec::new();

    for line in stderr.lines() {
        if line.contains("defined in header") || line.contains("include") {
            if let Some(start) = line.find('<') {
                let rest = &line[start + 1..];
                if let Some(end) = rest.find('>') {
                    let h = rest[..end].trim();
                    if !h.is_empty() && h.ends_with(".h") && !detected.contains(&h.to_string()) {
                        detected.push(h.to_string());
                    }
                }
            }
        }
    }

    detected.into_iter().filter(|h| !included_headers.contains(h)).collect()
}

fn is_var_already_declared(var_name: &str, existing_vars: &[String]) -> bool {
    for decl in existing_vars {
        let clean = decl.trim().trim_end_matches(';').trim();
        let lhs = if let Some(eq_pos) = clean.find('=') {
            clean[..eq_pos].trim()
        } else {
            clean
        };
        let before_bracket = lhs.split('[').next().unwrap_or(lhs).trim();
        let declared_name = before_bracket
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_start_matches('*')
            .trim_start_matches('&')
            .trim();

        if !declared_name.is_empty() && declared_name == var_name {
            if is_valid_c_declaration(clean) || clean.contains("struct ") {
                return true;
            }
        }
    }
    false
}

/// Parse compiler error output for undeclared variable messages
fn detect_undeclared_variables(stderr: &str, existing_vars: &[String]) -> Vec<(String, String)> {
    let mut vars = Vec::new();

    for line in stderr.lines() {
        if line.contains("undeclared") {
            if let Some(start_quote) = line.find('‘').or_else(|| line.find('\'')) {
                let rest = &line[start_quote + if line.as_bytes()[start_quote] == b'\'' { 1 } else { 3 }..];
                if let Some(end_quote) = rest.find('’').or_else(|| rest.find('\'')) {
                    let var_name = rest[..end_quote].trim();
                    if !var_name.is_empty() && var_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        let decl = infer_variable_declaration(var_name);
                        if !vars.iter().any(|(v, _)| v == var_name) && !is_var_already_declared(var_name, existing_vars) {
                            vars.push((var_name.to_string(), decl));
                        }
                    }
                }
            }
        }
    }

    vars
}

/// Infer a sensible C variable declaration based on variable name
fn infer_variable_declaration(var_name: &str) -> String {
    match var_name {
        "buf" | "buffer" | "path" | "pathname" | "str" | "filename" | "out" | "dest" | "src" => {
            format!("char {}[1024] = {{0}};", var_name)
        }
        "st" | "statbuf" | "stat" => {
            format!("struct stat {} = {{0}};", var_name)
        }
        "tv" | "timeval" => {
            format!("struct timeval {} = {{0}};", var_name)
        }
        "fds" | "pollfd" => {
            format!("struct pollfd {}[1] = {{0}};", var_name)
        }
        "len" | "size" | "count" | "n" => {
            format!("size_t {} = 1024;", var_name)
        }
        "fd" | "filedes" => {
            format!("int {} = 0;", var_name)
        }
        _ => {
            format!("char {}[1024] = {{0}};", var_name)
        }
    }
}

fn prompt_variable_declaration(vname: &str, default_decl: &str) -> String {
    if default_decl.contains("char ") && default_decl.contains('[') {
        let default_size = default_decl
            .split('[')
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("1024");

        print!(
            "  \x1b[93menter size for array '{}' [default: {}]: \x1b[0m",
            vname, default_size
        );
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if !trimmed.is_empty() {
                return format!("char {}[{}] = {{0}};", vname, trimmed);
            }
        }
        default_decl.to_string()
    } else if default_decl.contains("size_t ") {
        let default_val = default_decl
            .split('=')
            .nth(1)
            .and_then(|s| s.trim_end_matches(';').trim().parse::<usize>().ok())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "1024".to_string());

        print!(
            "  \x1b[93menter value for size_t '{}' [default: {}]: \x1b[0m",
            vname, default_val
        );
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if !trimmed.is_empty() {
                return format!("size_t {} = {};", vname, trimmed);
            }
        }
        default_decl.to_string()
    } else if default_decl.contains("int ") {
        let default_val = default_decl
            .split('=')
            .nth(1)
            .and_then(|s| s.trim_end_matches(';').trim().parse::<isize>().ok())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "0".to_string());

        print!(
            "  \x1b[93menter initial value for int '{}' [default: {}]: \x1b[0m",
            vname, default_val
        );
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if !trimmed.is_empty() {
                return format!("int {} = {};", vname, trimmed);
            }
        }
        default_decl.to_string()
    } else {
        default_decl.to_string()
    }
}

pub struct SessionRunner {
    pub extra_headers: Vec<String>,
    pub extra_vars: Vec<String>,
    pub statements: Vec<String>,
    pub focus_index: Option<usize>,
    pub daemon: Option<crate::daemon::DaemonManager>,
}

impl Clone for SessionRunner {
    fn clone(&self) -> Self {
        Self {
            extra_headers: self.extra_headers.clone(),
            extra_vars: self.extra_vars.clone(),
            statements: self.statements.clone(),
            focus_index: self.focus_index,
            daemon: None,
        }
    }
}

impl SessionRunner {
    pub fn new() -> Self {
        Self {
            extra_headers: Vec::new(),
            extra_vars: Vec::new(),
            statements: Vec::new(),
            focus_index: None,
            daemon: None,
        }
    }

    pub fn reset(&mut self) {
        self.extra_headers.clear();
        self.extra_vars.clear();
        self.statements.clear();
        self.focus_index = None;
        if let Some(mut d) = self.daemon.take() {
            d.shutdown();
        }
    }

    pub fn ensure_daemon(&mut self) -> Result<u32, String> {
        if self.daemon.is_none() {
            let dm = crate::daemon::DaemonManager::spawn()?;
            let pid = dm.pid;
            self.daemon = Some(dm);
            Ok(pid)
        } else {
            Ok(self.daemon.as_ref().unwrap().pid)
        }
    }
}

fn format_c_execution_block(expr: &str) -> (String, String) {
    let clean = expr.trim_end_matches(';').trim();
    if clean.is_empty() || clean == ";" {
        return ("\n".to_string(), String::new());
    }
    if is_valid_c_declaration(clean) || clean.contains('=') {
        let (var_name, stmt_str) = if let Some(eq_pos) = clean.find('=') {
            let lhs = clean[..eq_pos].trim();
            let before_bracket = lhs.split('[').next().unwrap_or(lhs);
            let var = before_bracket.split_whitespace().last().unwrap_or("").trim_start_matches('*').trim();
            (var.to_string(), format!("    {};\n", clean))
        } else {
            let before_bracket = clean.split('[').next().unwrap_or(clean);
            let var = before_bracket.split_whitespace().last().unwrap_or("").trim_start_matches('*').trim();
            (var.to_string(), format!("    {};\n", clean))
        };

        if !var_name.is_empty() && var_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return (
                stmt_str,
                format!(
                    r#"    errno = 0;
    long res = (long)({});
    if (res < 0 || errno != 0) {{
        printf("\n[kernel return value: %ld (errno %d: %s)]\n", res, errno, strerror(errno));
    }} else {{
        printf("\n[kernel return value: %ld]\n", res);
    }}
"#,
                    var_name
                ),
            );
        }
    }
    (
        String::new(),
        format!(
            r#"    errno = 0;
    long res = (long)({});
    if (res < 0 || errno != 0) {{
        printf("\n[kernel return value: %ld (errno %d: %s)]\n", res, errno, strerror(errno));
    }} else {{
        printf("\n[kernel return value: %ld]\n", res);
    }}
"#,
            clean
        ),
    )
}

fn is_focusable_expression(clean: &str) -> bool {
    // A focusable statement is one that produces a meaningful return value to check:
    // it must contain a function/syscall call (has '(') and not be a pure variable
    // declaration without a call (e.g. "char buf[4096]" or "int fd").
    if clean.is_empty() || clean == ";" {
        return false;
    }
    // Must contain a function call parenthesis
    if !clean.contains('(') {
        return false;
    }
    true
}

fn resolve_focus_index(session: &SessionRunner) -> Option<usize> {
    let total = session.statements.len();
    if total == 0 {
        return None;
    }

    // If user explicitly set a focus index, honour it (as long as index is valid & non-blank)
    if let Some(idx) = session.focus_index {
        if idx < total {
            let clean = session.statements[idx].trim_end_matches(';').trim();
            if !clean.is_empty() && clean != ";" {
                return Some(idx);
            }
        }
    }

    // First pass: prefer the last statement that is an actual function/syscall call
    for i in (0..total).rev() {
        let clean = session.statements[i].trim_end_matches(';').trim();
        if is_focusable_expression(clean) {
            return Some(i);
        }
    }

    // Second pass: fall back to last non-blank statement (e.g. all statements are declarations)
    for i in (0..total).rev() {
        let clean = session.statements[i].trim_end_matches(';').trim();
        if !clean.is_empty() && clean != ";" {
            return Some(i);
        }
    }

    None
}

/// C template — user expression slots into main(void) and prints kernel return value
fn build_c_source(expr: &str, extra_headers: &[String], extra_vars: &[String]) -> String {
    let mut includes = String::from("#include <unistd.h>\n#include <sys/syscall.h>\n#include <stdio.h>\n#include <errno.h>\n#include <string.h>\n");
    for h in extra_headers {
        if h != "errno.h" && h != "string.h" && h != "unistd.h" && h != "sys/syscall.h" && h != "stdio.h" {
            includes.push_str(&format!("#include <{}>\n", h));
        }
    }

    let mut vars_code = String::new();
    for v in extra_vars {
        vars_code.push_str(&format!("    {}\n", v));
    }

    let (stmt_code, eval_code) = format_c_execution_block(expr);

    format!(
        r#"{includes}
int main(void)
{{
{vars_code}{stmt_code}{eval_code}    return 0;
}}
"#,
        includes = includes.trim_end(),
        vars_code = vars_code,
        stmt_code = stmt_code,
        eval_code = eval_code
    )
}

fn build_c_session_source(session: &SessionRunner) -> String {
    let default_headers = vec![
        "unistd.h".to_string(),
        "sys/syscall.h".to_string(),
        "stdio.h".to_string(),
        "errno.h".to_string(),
        "string.h".to_string(),
    ];

    let mut includes = String::from("#include <unistd.h>\n#include <sys/syscall.h>\n#include <stdio.h>\n#include <errno.h>\n#include <string.h>\n");
    for h in &session.extra_headers {
        if !default_headers.contains(h) {
            includes.push_str(&format!("#include <{}>\n", h));
        }
    }

    let mut vars_code = String::new();
    for v in &session.extra_vars {
        vars_code.push_str(&format!("    {}\n", v));
    }

    let mut stmts_code = String::new();
    let focus_opt = resolve_focus_index(session);

    for (i, st) in session.statements.iter().enumerate() {
        let clean = st.trim_end_matches(';').trim();
        if Some(i) == focus_opt {
            let (stmt_code, eval_code) = format_c_execution_block(clean);
            stmts_code.push_str(&stmt_code);
            stmts_code.push_str(&eval_code);
            break;
        } else {
            if !clean.is_empty() && clean != ";" {
                stmts_code.push_str(&format!("    {};\n", clean));
            } else {
                stmts_code.push_str("\n");
            }
        }
    }

    format!(
        r#"{includes}
int main(void)
{{
{vars_code}{stmts_code}    return 0;
}}
"#,
        includes = includes.trim_end(),
        vars_code = vars_code,
        stmts_code = stmts_code
    )
}

/// Returns line information for each statement in the generated C box preview:
/// (statement_index, start_c_line, end_c_line, statement_str, is_focused)
pub fn get_statement_lines(session: &SessionRunner) -> Vec<(usize, usize, usize, String, bool)> {
    let default_headers = vec![
        "unistd.h".to_string(),
        "sys/syscall.h".to_string(),
        "stdio.h".to_string(),
        "errno.h".to_string(),
        "string.h".to_string(),
    ];

    let mut header_count = 5;
    for h in &session.extra_headers {
        if !default_headers.contains(h) {
            header_count += 1;
        }
    }

    let prelude_lines = header_count + 1 /* int main */ + 1 /* { */ + session.extra_vars.len();

    let focus_opt = resolve_focus_index(session);

    let mut current_line = prelude_lines + 1;
    let mut info = Vec::new();

    for (i, st) in session.statements.iter().enumerate() {
        let clean = st.trim_end_matches(';').trim();
        let is_focused = Some(i) == focus_opt;
        let line_count = if is_focused {
            // Compute the exact number of C lines this focused statement generates
            let (sc, ec) = format_c_execution_block(clean);
            let sc_lines = sc.lines().count();
            let ec_lines = ec.lines().count();
            // Empty stmt_code (direct call) contributes 0 lines; non-empty (declaration) contributes sc_lines
            let sc_contribution = if sc.trim().is_empty() { 0 } else { sc_lines };
            (sc_contribution + ec_lines).max(1)
        } else if clean.is_empty() || clean == ";" {
            1 // blank line renders as a single empty line
        } else {
            1
        };
        let start = current_line;
        let end = current_line + line_count - 1;
        info.push((i, start, end, clean.to_string(), is_focused));
        current_line += line_count;
        if is_focused {
            break;
        }
    }

    info
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTarget {
    Statement(usize),
    InsertAfter(usize),
    ExtraVar(usize),
    Header(usize),
}

pub fn resolve_c_line_target(num: usize, session: &SessionRunner) -> LineTarget {
    let lines_info = get_statement_lines(session);
    let default_headers = vec![
        "unistd.h".to_string(),
        "sys/syscall.h".to_string(),
        "stdio.h".to_string(),
        "errno.h".to_string(),
        "string.h".to_string(),
    ];
    let mut header_count = 5;
    for h in &session.extra_headers {
        if !default_headers.contains(h) {
            header_count += 1;
        }
    }
    let var_start = header_count + 3; // int main + { + first var line
    let var_end = if session.extra_vars.is_empty() { var_start } else { var_start + session.extra_vars.len() - 1 };

    for (idx, start, end, _, _) in &lines_info {
        if num >= *start && num <= *end {
            return LineTarget::Statement(*idx);
        }
    }

    if !session.extra_vars.is_empty() && num >= var_start && num <= var_end {
        return LineTarget::ExtraVar(num - var_start);
    }

    if num >= 1 && num <= header_count {
        return LineTarget::Header(num - 1);
    }

    if let Some(last_info) = lines_info.last() {
        if num > last_info.2 {
            return LineTarget::InsertAfter(session.statements.len());
        }
        if num < lines_info[0].1 {
            return LineTarget::InsertAfter(0);
        }
    }

    if num >= 1 && num <= session.statements.len() {
        return LineTarget::Statement(num - 1);
    }

    LineTarget::InsertAfter(session.statements.len())
}

/// Parse focus command input (e.g., "L11", "11", "1", "reset") and return target statement index or None
pub fn parse_focus_input(input_str: &str, session: &SessionRunner) -> Result<Option<usize>, String> {
    let s = input_str.trim();
    let lower = s.to_lowercase();
    if lower == "reset" || lower == "default" || lower == "last" || lower == "latest" {
        return Ok(None);
    }

    let clean_num = lower.trim_start_matches('l').trim();
    if let Ok(num) = clean_num.parse::<usize>() {
        let lines_info = get_statement_lines(session);
        let total = session.statements.len();

        if lower.starts_with('l') {
            for (idx, start, end, _, _) in &lines_info {
                if num >= *start && num <= *end {
                    return Ok(Some(*idx));
                }
            }
            if num >= 1 && num <= total {
                return Ok(Some(num - 1));
            }
            return Err(format!("C line L{} does not correspond to any active statement/syscall.", num));
        } else {
            if num >= 1 && num <= total {
                return Ok(Some(num - 1));
            }
            for (idx, start, end, _, _) in &lines_info {
                if num >= *start && num <= *end {
                    return Ok(Some(*idx));
                }
            }
            return Err(format!("invalid statement or C line number {}.", num));
        }
    }

    Err("invalid focus target. Provide a statement number (e.g. 1), C line (e.g. L11), or 'reset'".to_string())
}

/// Heuristic: does this value look like a user-space virtual address on Linux x86_64?
/// User-space addresses are typically 0x10000..0x7FFFFFFFFFFF.
/// Small integers (fds, counts, errno values) are filtered out.
fn looks_like_address(val: i64) -> bool {
    let u = val as u64;
    // Must be > 64KB (rules out small fds/counts) and below kernel boundary
    u >= 0x0001_0000 && u <= 0x0000_7FFF_FFFF_FFFF
}

/// Parse the program's stdout for the "[kernel return value: N]" line and extract N.
fn parse_return_value(output: &str) -> Option<i64> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[kernel return value:") {
            // Extract the first integer after the colon
            let after_colon = trimmed
                .trim_start_matches("[kernel return value:")
                .trim();
            let num_str: String = after_colon
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(n) = num_str.parse::<i64>() {
                return Some(n);
            }
        }
    }
    None
}

/// Build a C program that safely inspects a memory address returned by a syscall.
/// Uses setjmp/longjmp to catch SIGSEGV/SIGBUS if the pointer is unmappable.
fn build_memory_inspector_c(
    expr: &str,
    extra_headers: &[String],
    extra_vars: &[String],
) -> String {
    let mut includes = String::from(
        "#include <unistd.h>\n\
         #include <sys/syscall.h>\n\
         #include <stdio.h>\n\
         #include <errno.h>\n\
         #include <string.h>\n\
         #include <stdint.h>\n\
         #include <ctype.h>\n\
         #include <setjmp.h>\n\
         #include <signal.h>\n\
         #include <stdlib.h>\n",
    );
    let default_skip = ["errno.h","string.h","unistd.h","sys/syscall.h","stdio.h",
                        "stdint.h","ctype.h","setjmp.h","signal.h","stdlib.h"];
    for h in extra_headers {
        if !default_skip.contains(&h.as_str()) {
            includes.push_str(&format!("#include <{}>\n", h));
        }
    }

    let mut vars_code = String::new();
    for v in extra_vars {
        vars_code.push_str(&format!("    {}\n", v));
    }

    let clean_expr = expr.trim_end_matches(';').trim();

    format!(r#"{includes}
static sigjmp_buf _scm_safe;
static void _scm_segv(int s) {{ (void)s; siglongjmp(_scm_safe, 1); }}
static void _install_handler(void) {{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = _scm_segv;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS,  &sa, NULL);
}}

static void print_maps_region(unsigned long addr) {{
    FILE *m = fopen("/proc/self/maps", "r");
    if (!m) return;
    char line[512];
    while (fgets(line, sizeof(line), m)) {{
        unsigned long s = 0, e = 0;
        if (sscanf(line, "%lx-%lx", &s, &e) == 2 && addr >= s && addr < e) {{
            printf("  [maps region]  %s", line);
            fclose(m);
            return;
        }}
    }}
    fclose(m);
    printf("  [maps region]  not found in /proc/self/maps\n");
}}

static void hex_dump(const unsigned char *p, size_t n) {{
    for (size_t i = 0; i < n; i++) {{
        if (i % 16 == 0) printf("\n    %04zx: ", i);
        printf("%02x ", p[i]);
        if ((i + 1) % 16 == 0) {{
            printf(" | ");
            for (size_t j = i - 15; j <= i; j++)
                printf("%c", isprint(p[j]) ? (char)p[j] : '.');
        }}
    }}
    /* trailing ascii for partial last row */
    size_t rem = n % 16;
    if (rem != 0) {{
        for (size_t i = 0; i < (16 - rem); i++) printf("   ");
        printf(" | ");
        for (size_t i = n - rem; i < n; i++)
            printf("%c", isprint(p[i]) ? (char)p[i] : '.');
    }}
    printf("\n");
}}

int main(void)
{{
{vars_code}    errno = 0;
    long res = (long)({expr});
    if (res < 0 || errno != 0) {{
        printf("\n  [memory inspector] syscall returned error: %ld (errno %d: %s)\n",
               res, errno, strerror(errno));
        return 1;
    }}

    void *ptr     = (void *)(uintptr_t)res;
    unsigned long uaddr = (unsigned long)(uintptr_t)ptr;

    printf("\n  ┌─── memory inspector ─────────────────────────────────────\n");
    printf("  │  address  : %p\n", ptr);
    printf("  │  hex      : 0x%016lx\n", uaddr);

    /* ── /proc/self/maps region ── */
    print_maps_region(uaddr);

    /* ── String interpretation ── */
    _install_handler();
    if (sigsetjmp(_scm_safe, 1) == 0) {{
        const char *s = (const char *)ptr;
        size_t slen = strnlen(s, 4096);
        int looks_str = slen > 0 && slen < 4096;
        /* check all printable */
        int all_print = 1;
        for (size_t i = 0; i < slen; i++) if (!isprint((unsigned char)s[i]) && s[i] != '\n' && s[i] != '\t') {{ all_print = 0; break; }}
        if (looks_str && all_print) {{
            printf("  │  type hint : null-terminated C string (%zu bytes)\n", slen);
            printf("  │  value     : \"%.*s\"\n", (int)slen, s);
        }} else {{
            printf("  │  type hint : not a clean C string (first %zu bytes read)\n", slen);
        }}
    }} else {{
        printf("  │  string    : SIGSEGV — address not readable\n");
    }}

    /* ── Hex dump (first 64 bytes) ── */
    _install_handler();
    if (sigsetjmp(_scm_safe, 1) == 0) {{
        const unsigned char *b = (const unsigned char *)ptr;
        printf("  │  hex dump  : (up to 64 bytes)");
        hex_dump(b, 64);
    }} else {{
        printf("  │  hex dump  : SIGSEGV — address not readable\n");
    }}

    printf("  └───────────────────────────────────────────────────────────\n");
    return 0;
}}
"#,
        includes = includes.trim_end(),
        vars_code = vars_code,
        expr = clean_expr,
    )
}

/// Compile and run the memory inspector for a given return value / expression.
fn run_memory_inspector(
    expr: &str,
    extra_headers: &[String],
    extra_vars: &[String],
    compiler: &str,
) {
    let source = build_memory_inspector_c(expr, extra_headers, extra_vars);
    let tmp = std::env::temp_dir();
    let src = tmp.join("scm_memview.c");
    let bin = tmp.join("scm_memview");

    if fs::write(&src, &source).is_err() {
        eprintln!("  {RED}✗  could not write memory inspector source{RESET}", RED=RED, RESET=RESET);
        return;
    }

    println!("  {CYAN}compiling memory inspector …{RESET}", CYAN=CYAN, RESET=RESET);
    let compile = Command::new(compiler)
        .args([src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .output();

    match compile {
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!("  {RED}✗  inspector compilation failed:{RESET}", RED=RED, RESET=RESET);
            for line in stderr.lines() {
                eprintln!("    {DIM}{}{RESET}", line, DIM=DIM, RESET=RESET);
            }
            let _ = fs::remove_file(&src);
            return;
        }
        Err(e) => {
            eprintln!("  {RED}✗  {}{RESET}", e, RED=RED, RESET=RESET);
            let _ = fs::remove_file(&src);
            return;
        }
        Ok(_) => {}
    }

    let run = Command::new(&bin).output();
    match run {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            for line in stdout.lines() {
                println!("  {CYAN}│{RESET}  {}", line, CYAN=CYAN, RESET=RESET);
            }
            for line in stderr.lines() {
                eprintln!("  {RED}│{RESET}  {}", line, RED=RED, RESET=RESET);
            }
        }
        Err(e) => eprintln!("  {RED}✗  {}{RESET}", e, RED=RED, RESET=RESET),
    }

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&bin);
}


/// Print the C source with line numbers and syntax highlighting (basic)
pub fn print_source(source: &str) {
    println!("{BOLD}  generated C program:{RESET}", BOLD=BOLD, RESET=RESET);
    println!("  {DIM}┌{}{RESET}", "─".repeat(60), DIM=DIM, RESET=RESET);
    for (i, line) in source.lines().enumerate() {
        let ln = i + 1;
        let line_trim = line.trim();
        let coloured = if line_trim.starts_with("#include") {
            format!("  {DIM}│{RESET}{DIM}{:3} │{RESET}  {MAGENTA}{}{RESET}",
                ln, line, DIM=DIM, RESET=RESET, MAGENTA="\x1b[95m")
        } else if line_trim.starts_with("long res") || (line_trim.contains('(') && !line_trim.starts_with("int main") && !line_trim.starts_with("if") && !line_trim.starts_with("printf")) {
            format!("  {DIM}│{RESET}{DIM}{:3} │{RESET}  {CYAN}{}{RESET}",
                ln, line, DIM=DIM, RESET=RESET, CYAN=CYAN)
        } else {
            format!("  {DIM}│{RESET}{DIM}{:3} │{RESET}  {BLUE}{}{RESET}",
                ln, line, DIM=DIM, RESET=RESET, BLUE=BLUE)
        };
        println!("{}", coloured);
    }
    println!("  {DIM}└{}{RESET}", "─".repeat(60), DIM=DIM, RESET=RESET);
}

/// Print current code space for session
pub fn print_codespace(session: &SessionRunner) {
    if session.statements.is_empty() && session.extra_vars.is_empty() {
        println!("  {YELLOW}code space is currently empty.{RESET}\n", YELLOW=YELLOW, RESET=RESET);
        return;
    }
    let source = build_c_session_source(session);
    println!();
    print_source(&source);
    println!();
}



/// Find a C compiler: user-specified > gcc > clang
fn find_compiler(specified: Option<&str>) -> Result<String, String> {
    if let Some(c) = specified {
        if Command::new(c).arg("--version").output().is_ok() {
            return Ok(c.to_string());
        }
        return Err(format!("specified compiler not found: {}", c));
    }
    for candidate in &["gcc", "clang", "cc"] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Ok(candidate.to_string());
        }
    }
    Err("no C compiler found — install gcc or clang".to_string())
}

/// Ask user to confirm execution (Enter to run, q to cancel)
fn confirm_run(prompt: &str) -> bool {
    print!("{}", prompt);
    io::stdout().flush().unwrap();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let trimmed = line.trim().to_lowercase();
    !matches!(trimmed.as_str(), "q" | "quit" | "n" | "no")
}

/// Ask the user yes/no, return true for yes
fn confirm(prompt: &str) -> bool {
    confirm_run(prompt)
}

#[derive(Default, Debug, Clone, Copy)]
pub struct InlineFlags {
    pub editor_mode: Option<bool>,
    pub auto_confirm: Option<bool>,
    pub mem_inspect: Option<bool>,
    pub runner_session: Option<bool>,
    pub quick_mode: Option<bool>,
}

pub fn extract_inline_flags(raw: &str) -> (String, InlineFlags) {
    let mut clean = raw.trim().to_string();
    let mut flags = InlineFlags::default();

    loop {
        let trimmed = clean.trim();
        let mut matched = false;
        for flag_str in &[
            " -m", " --mem-inspect",
            " -e", " --editor",
            " -y", " --yes",
            " -R", " --runner", " --runner-session",
            " -q", " --quick",
        ] {
            if trimmed.ends_with(flag_str) {
                let cutoff = trimmed.len() - flag_str.len();
                clean = trimmed[..cutoff].to_string();
                match flag_str.trim() {
                    "-m" | "--mem-inspect" => flags.mem_inspect = Some(true),
                    "-e" | "--editor" => flags.editor_mode = Some(true),
                    "-y" | "--yes" => flags.auto_confirm = Some(true),
                    "-R" | "--runner" | "--runner-session" => flags.runner_session = Some(true),
                    "-q" | "--quick" => flags.quick_mode = Some(true),
                    _ => {}
                }
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }

    (clean, flags)
}

/// Parse line commands ('rpc' or 'ins').
pub fn parse_line_command(input: &str, cmd_prefix: &str) -> Option<Vec<(usize, String)>> {
    let s = input.trim();
    if !s.to_lowercase().starts_with(&cmd_prefix.to_lowercase()) {
        return None;
    }

    let rest = s[cmd_prefix.len()..].trim();
    if rest.is_empty() {
        return Some(Vec::new());
    }

    let rest_lower = rest.to_lowercase();
    if rest_lower == "l*" || rest_lower == "l *" || rest_lower.starts_with("l*") || rest_lower.starts_with("l *") {
        return Some(vec![(usize::MAX, "NULL".to_string())]);
    }

    let mut matches: Vec<(usize, usize, usize)> = Vec::new();
    let bytes = rest.as_bytes();
    let len = bytes.len();
    let mut idx = 0;

    while idx < len {
        if (bytes[idx] == b'L' || bytes[idx] == b'l') && idx + 1 < len && bytes[idx + 1].is_ascii_digit() {
            if idx == 0 || bytes[idx - 1].is_ascii_whitespace() {
                let num_start = idx + 1;
                let mut num_end = num_start;
                while num_end < len && bytes[num_end].is_ascii_digit() {
                    num_end += 1;
                }
                if num_end == len || bytes[num_end].is_ascii_whitespace() || bytes[num_end] == b'/' || bytes[num_end] == b'\\' {
                    if let Ok(line_num) = rest[num_start..num_end].parse::<usize>() {
                        matches.push((idx, num_end, line_num));
                        idx = num_end;
                        continue;
                    }
                }
            }
        }
        idx += 1;
    }

    if matches.is_empty() {
        return None;
    }

    let mut replacements = Vec::new();
    for i in 0..matches.len() {
        let (_, num_end, line_num) = matches[i];
        let content_end = if i + 1 < matches.len() {
            matches[i + 1].0
        } else {
            len
        };
        let code_segment = rest[num_end..content_end].trim();
        replacements.push((line_num, code_segment.to_string()));
    }

    Some(replacements)
}

pub fn parse_rpc_command(input: &str) -> Option<Vec<(usize, String)>> {
    parse_line_command(input, "rpc")
}

/// Save updated session after applying rpc/ins/fc code line edits with save/discard prompt (no auto-run)
pub fn run_session_rerun_rpc(
    session: &mut SessionRunner,
    old_session: SessionRunner,
    quick_mode: bool,
    editor_mode: bool,
    auto_confirm: bool,
    _mem_inspect: bool,
    _compiler_path: Option<&str>,
) {
    if session.statements.is_empty() && session.extra_vars.is_empty() {
        return;
    }

    let source = build_c_session_source(session);

    if !quick_mode || editor_mode {
        println!();
        print_source(&source);
        println!();
    }

    let prompt = format!(
        "  {YELLOW}save changes to code space?{RESET} [Enter to save / q to discard]  ",
        YELLOW=YELLOW, RESET=RESET
    );

    let ok = if auto_confirm {
        true
    } else {
        confirm_run(&prompt)
    };

    if !ok {
        println!("  {DIM}changes discarded.{RESET}", DIM=DIM, RESET=RESET);
        *session = old_session;
    } else {
        println!("  {GREEN}✓  code space updated (use 'cs run' to execute){RESET}", GREEN=GREEN, RESET=RESET);
    }
}

/// Directly compile and run the current session code space
pub fn run_session_direct(
    session: &mut SessionRunner,
    quick_mode: bool,
    editor_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<&str>,
) {
    if session.statements.is_empty() && session.extra_vars.is_empty() {
        println!("  {YELLOW}code space is currently empty. Add statements or syscalls first.{RESET}\n", YELLOW=YELLOW, RESET=RESET);
        return;
    }

    let target_idx = resolve_focus_index(session).unwrap_or(0);
    let stmt_to_run = if target_idx < session.statements.len() {
        session.statements[target_idx].clone()
    } else {
        session.statements.last().cloned().unwrap_or_default()
    };

    let clean = stmt_to_run.trim_end_matches(';').trim();
    if clean.is_empty() || clean == ";" {
        if !quick_mode {
            print_codespace(session);
        }
        return;
    }

    let mut source = build_c_session_source(session);

    if !quick_mode || editor_mode {
        println!();
        print_source(&source);
        println!();
    }

    if editor_mode {
        let mut scm_editor = crate::editor::ScmEditor::new(&source);
        match scm_editor.run() {
            crate::editor::EditorResult::Save(edited_code) => {
                source = edited_code;
                println!();
                println!("  {GREEN}✓  updated C source from built-in editor:{RESET}", GREEN=GREEN, RESET=RESET);
                if !quick_mode || editor_mode {
                    print_source(&source);
                    println!();
                }
            }
            crate::editor::EditorResult::Cancel => {
                println!("  {DIM}editing cancelled.{RESET}", DIM=DIM, RESET=RESET);
                return;
            }
        }
    }

    let prompt = format!(
        "  {YELLOW}compile and run current session?{RESET} [Enter to run / q to cancel]  ",
        YELLOW=YELLOW, RESET=RESET
    );

    let ok = if auto_confirm || editor_mode || quick_mode {
        true
    } else {
        confirm_run(&prompt)
    };

    if !ok {
        println!("  {DIM}aborted.{RESET}", DIM=DIM, RESET=RESET);
        return;
    }

    let compiler = match find_compiler(compiler_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  {RED}✗  {}{RESET}", e, RED=RED, RESET=RESET);
            return;
        }
    };

    let tmp_dir = std::env::temp_dir();
    let src_path = tmp_dir.join("scm_syscall.c");
    let bin_path = tmp_dir.join("scm_syscall");

    if !quick_mode {
        println!();
        println!("  {DIM}compiler: {}{RESET}", compiler, DIM=DIM, RESET=RESET);
        println!("  {DIM}source:   {}{RESET}", src_path.display(), DIM=DIM, RESET=RESET);
        println!("  {DIM}binary:   {}{RESET}", bin_path.display(), DIM=DIM, RESET=RESET);
        println!();
    }

    let mut current_source = source.clone();
    let default_headers = vec![
        "unistd.h".to_string(), "sys/syscall.h".to_string(),
        "stdio.h".to_string(), "errno.h".to_string(), "string.h".to_string(),
    ];

    loop {
        fs::write(&src_path, &current_source).expect("failed to write temp C file");
        if !quick_mode {
            println!("  {CYAN}compiling …{RESET}", CYAN=CYAN, RESET=RESET);
        }
        let compile_out = Command::new(&compiler)
            .args([src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
            .output();

        match compile_out {
            Err(e) => {
                eprintln!("  {RED}✗  compiler error: {}{RESET}", e, RED=RED, RESET=RESET);
                let _ = fs::remove_file(&src_path);
                return;
            }
            Ok(out) if !out.status.success() => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("  {RED}✗  compilation failed:{RESET}", RED=RED, RESET=RESET);
                for line in stderr.lines() {
                    eprintln!("    {DIM}{}{RESET}", line, DIM=DIM, RESET=RESET);
                }
                let mut all_headers = default_headers.clone();
                all_headers.extend(session.extra_headers.clone());
                let mut all_vars = session.extra_vars.clone();
                all_vars.extend(session.statements.clone());
                for line in current_source.lines() { all_vars.push(line.to_string()); }

                let missing_headers = detect_missing_headers(&stderr, &all_headers);
                let missing_vars = detect_undeclared_variables(&stderr, &all_vars);

                if !missing_headers.is_empty() || !missing_vars.is_empty() {
                    println!();
                    if !missing_headers.is_empty() {
                        let s = missing_headers.iter().map(|h| format!("<{}>", h)).collect::<Vec<_>>().join(", ");
                        println!("  {YELLOW}missing header(s) detected: {}{RESET}", s, YELLOW=YELLOW, RESET=RESET);
                    }
                    if !missing_vars.is_empty() {
                        for (vname, decl) in &missing_vars {
                            println!("  {YELLOW}undeclared variable: '{vname}' (suggested: {decl}){RESET}", vname=vname, decl=decl, YELLOW=YELLOW, RESET=RESET);
                        }
                    }
                    let fix_prompt = format!("  {YELLOW}apply auto-fixes and try again?{RESET} [Y/n]  ", YELLOW=YELLOW, RESET=RESET);
                    let apply_fix = if auto_confirm || quick_mode { true } else { confirm(&fix_prompt) };
                    if apply_fix {
                        let customized_vars: Vec<String> = missing_vars.iter()
                            .map(|(vname, decl)| if auto_confirm || quick_mode { decl.clone() } else { prompt_variable_declaration(vname, decl) })
                            .collect();
                        for h in missing_headers { session.extra_headers.push(h); }
                        for decl in customized_vars { session.extra_vars.push(decl); }
                        current_source = build_c_session_source(session);
                        if !quick_mode {
                            println!("  {CYAN}re-building with fixes …{RESET}", CYAN=CYAN, RESET=RESET);
                        }
                        continue;
                    }
                }
                let _ = fs::remove_file(&src_path);
                return;
            }
            Ok(_) => {
                if !quick_mode {
                    println!("  {GREEN}✓  compiled{RESET}\n", GREEN=GREEN, RESET=RESET);
                }
                break;
            }
        }
    }

    if !quick_mode {
        println!("  {CYAN}running …{RESET}\n", CYAN=CYAN, RESET=RESET);
        println!("  {DIM}─── output ──────────────────────────────────────{RESET}", DIM=DIM, RESET=RESET);
    }

    let run_out = Command::new(&bin_path).output();
    match run_out {
        Err(e) => eprintln!("  {RED}✗  run error: {}{RESET}", e, RED=RED, RESET=RESET),
        Ok(out) => {
            let stdout_str = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&out.stderr);
            if !stdout_str.is_empty() {
                for line in stdout_str.lines() {
                    println!("  {GREEN}│{RESET}  {}", line, GREEN=GREEN, RESET=RESET);
                }
            }
            if !stderr_str.is_empty() {
                for line in stderr_str.lines() {
                    eprintln!("  {RED}│{RESET}  {}", line, RED=RED, RESET=RESET);
                }
            }
            let exit_code = out.status.code().unwrap_or(-1);
            if !quick_mode {
                println!("  {DIM}─── exit: {} ────────────────────────────────────{RESET}\n", exit_code, DIM=DIM, RESET=RESET);
                if out.status.success() {
                    println!("  {GREEN}✓  syscall executed{RESET}", GREEN=GREEN, RESET=RESET);
                } else {
                    eprintln!("  {RED}✗  process exited with code {}{RESET}", exit_code, RED=RED, RESET=RESET);
                }
            }

            // Memory address detection
            if let Some(ret_val) = parse_return_value(&stdout_str) {
                if looks_like_address(ret_val) {
                    println!();
                    println!(
                        "  {YELLOW}⚑  kernel returned 0x{:016x} ({}) — looks like a memory address!{RESET}",
                        ret_val as u64, ret_val, YELLOW=YELLOW, RESET=RESET
                    );
                    let do_inspect = mem_inspect || auto_confirm || quick_mode || confirm(&format!(
                        "  {YELLOW}inspect memory at that address?{RESET} [Y/n]  ",
                        YELLOW=YELLOW, RESET=RESET
                    ));
                    if do_inspect {
                        run_memory_inspector(
                            &stmt_to_run,
                            &session.extra_headers,
                            &session.extra_vars,
                            &compiler,
                        );
                    }
                }
            }
        }
    }

    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&bin_path);
    println!();
}

#[allow(dead_code)]
pub fn run_session_rerun(
    session: &mut SessionRunner,
    quick_mode: bool,
    editor_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<&str>,
) {
    if session.statements.is_empty() {
        return;
    }
    let last_stmt = session.statements.pop().unwrap_or_default();
    run(&last_stmt, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, Some(session));
}

/// Main entry — build C, show, optionally edit, confirm, compile, run
pub fn run(
    user_args: &str,
    quick_mode: bool,
    editor_mode: bool,
    auto_confirm: bool,
    mem_inspect: bool,
    compiler_path: Option<&str>,
    session: Option<&mut SessionRunner>,
) {
    run_with_custom_prompt(user_args, quick_mode, editor_mode, auto_confirm, mem_inspect, compiler_path, session, None);
}

pub fn run_with_custom_prompt(
    user_args: &str,
    mut quick_mode: bool,
    mut editor_mode: bool,
    mut auto_confirm: bool,
    mut mem_inspect: bool,
    compiler_path: Option<&str>,
    mut session: Option<&mut SessionRunner>,
    custom_prompt: Option<&str>,
) -> bool {
    let (clean_args, inline_flags) = extract_inline_flags(user_args);
    if let Some(v) = inline_flags.quick_mode { quick_mode = v; }
    if let Some(v) = inline_flags.editor_mode { editor_mode = v; }
    if let Some(v) = inline_flags.auto_confirm { auto_confirm = v; }
    if let Some(v) = inline_flags.mem_inspect { mem_inspect = v; }

    // ── Step 0: Prepare C statement ─────────────────────────
    let stmt = match prepare_c_statement(&clean_args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\n  {RED}✗  {}{RESET}\n", e, RED=RED, RESET=RESET);
            return false;
        }
    };

    let default_headers = vec![
        "unistd.h".to_string(),
        "sys/syscall.h".to_string(),
        "stdio.h".to_string(),
        "errno.h".to_string(),
        "string.h".to_string(),
    ];

    let mut local_headers: Vec<String> = Vec::new();
    let mut local_vars: Vec<String> = Vec::new();

    // Proactively query man pages for headers before generating C code
    if let Some(func_name) = extract_function_name(&clean_args) {
        let man_headers = lookup_headers_from_man(&func_name);
        for h in man_headers {
            if !default_headers.contains(&h) {
                if let Some(ref mut sess) = session {
                    if !sess.extra_headers.contains(&h) {
                        if !quick_mode {
                            println!("  {DIM}man lookup:{RESET} found header {CYAN}<{}>{RESET} for {}{RESET}", h, func_name, DIM=DIM, RESET=RESET, CYAN=CYAN);
                        }
                        sess.extra_headers.push(h);
                    }
                } else if !local_headers.contains(&h) {
                    if !quick_mode {
                        println!("  {DIM}man lookup:{RESET} found header {CYAN}<{}>{RESET} for {}{RESET}", h, func_name, DIM=DIM, RESET=RESET, CYAN=CYAN);
                    }
                    local_headers.push(h);
                }
            }
        }
    }

    if let Some(ref mut sess) = session {
        sess.statements.push(stmt.clone());
    }

    let tmp_dir = std::env::temp_dir();
    let src_path: PathBuf = tmp_dir.join("scm_syscall.c");
    let bin_path: PathBuf = tmp_dir.join("scm_syscall");

    loop {
        let mut source = if let Some(ref sess) = session {
            build_c_session_source(sess)
        } else {
            build_c_source(&stmt, &local_headers, &local_vars)
        };

        let daemon_pid = if let Some(ref mut sess) = session {
            match sess.ensure_daemon() {
                Ok(pid) => Some(pid),
                Err(e) => {
                    if !quick_mode {
                        eprintln!("  {RED}✗  daemon spawn failed: {}{RESET}", e, RED=RED, RESET=RESET);
                    }
                    None
                }
            }
        } else {
            None
        };

        let is_so = daemon_pid.is_some();
        let target_bin = if is_so {
            tmp_dir.join(format!("scm_snippet_{}.so", crate::daemon::rand_num()))
        } else {
            bin_path.clone()
        };

        let mut build_source = source.clone();
        if is_so {
            build_source = build_source.replace("int main(void)", "long scm_exec(void)");
        }

        fs::write(&src_path, &build_source).expect("failed to write temp C file");

        if !quick_mode || editor_mode {
            println!();
            print_source(&source);
            println!();
        }

        if editor_mode {
            let mut scm_editor = crate::editor::ScmEditor::new(&source);
            match scm_editor.run() {
                crate::editor::EditorResult::Save(edited_code) => {
                    source = edited_code;
                    build_source = source.clone();
                    if is_so {
                        build_source = build_source.replace("int main(void)", "long scm_exec(void)");
                    }
                    fs::write(&src_path, &build_source).expect("failed to write edited C file");
                    println!();
                    println!("  {GREEN}✓  updated C source from built-in editor:{RESET}", GREEN=GREEN, RESET=RESET);
                    if !quick_mode || editor_mode {
                        print_source(&source);
                        println!();
                    }
                }
                crate::editor::EditorResult::Cancel => {
                    println!("  {DIM}editing cancelled.{RESET}", DIM=DIM, RESET=RESET);
                    if let Some(ref mut sess) = session {
                        sess.statements.pop();
                    }
                    let _ = fs::remove_file(&src_path);
                    return false;
                }
            }
        }

        let default_prompt = format!(
            "  {YELLOW}compile and run?{RESET} [Enter to run / q to cancel]  ",
            YELLOW=YELLOW, RESET=RESET
        );
        let prompt_text = custom_prompt.unwrap_or(&default_prompt);

        let ok = if auto_confirm || editor_mode || quick_mode {
            true
        } else {
            confirm_run(prompt_text)
        };

        if !ok {
            if custom_prompt.is_some() {
                println!("  {DIM}changes discarded.{RESET}", DIM=DIM, RESET=RESET);
            } else {
                println!("  {DIM}aborted.{RESET}", DIM=DIM, RESET=RESET);
            }
            if let Some(ref mut sess) = session {
                sess.statements.pop();
            }
            let _ = fs::remove_file(&src_path);
            return false;
        }

        let compiler = match find_compiler(compiler_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  {RED}✗  {}{RESET}", e, RED=RED, RESET=RESET);
                if let Some(ref mut sess) = session {
                    sess.statements.pop();
                }
                return false;
            }
        };

        if !quick_mode {
            println!();
            println!("  {DIM}compiler: {}{RESET}", compiler, DIM=DIM, RESET=RESET);
            println!("  {DIM}source:   {}{RESET}", src_path.display(), DIM=DIM, RESET=RESET);
            println!("  {DIM}target:   {}{RESET}", target_bin.display(), DIM=DIM, RESET=RESET);
            println!();
            println!("  {CYAN}compiling …{RESET}", CYAN=CYAN, RESET=RESET);
        }

        let compile = if is_so {
            Command::new(&compiler)
                .args([
                    "-shared", "-fPIC", "-rdynamic",
                    src_path.to_str().unwrap(),
                    "-o", target_bin.to_str().unwrap(),
                ])
                .output()
        } else {
            Command::new(&compiler)
                .args([
                    src_path.to_str().unwrap(),
                    "-o", target_bin.to_str().unwrap(),
                ])
                .output()
        };

        match compile {
            Err(e) => {
                eprintln!("  {RED}✗  compiler error: {}{RESET}", e, RED=RED, RESET=RESET);
                if let Some(ref mut sess) = session {
                    sess.statements.pop();
                }
                let _ = fs::remove_file(&src_path);
                return false;
            }
            Ok(out) if !out.status.success() => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("  {RED}✗  compilation failed:{RESET}", RED=RED, RESET=RESET);
                for line in stderr.lines() {
                    eprintln!("    {DIM}{}{RESET}", line, DIM=DIM, RESET=RESET);
                }

                let (current_all, existing_vars) = if let Some(ref sess) = session {
                    let mut all = default_headers.clone();
                    all.extend(sess.extra_headers.clone());
                    let mut vars = sess.extra_vars.clone();
                    vars.extend(sess.statements.clone());
                    for line in source.lines() {
                        vars.push(line.to_string());
                    }
                    (all, vars)
                } else {
                    let mut all = default_headers.clone();
                    all.extend(local_headers.clone());
                    let mut vars = local_vars.clone();
                    for line in source.lines() {
                        vars.push(line.to_string());
                    }
                    (all, vars)
                };

                let missing_headers = detect_missing_headers(&stderr, &current_all);
                let missing_vars = detect_undeclared_variables(&stderr, &existing_vars);

                if !missing_headers.is_empty() || !missing_vars.is_empty() {
                    println!();
                    if !missing_headers.is_empty() {
                        let missing_str = missing_headers.iter().map(|h| format!("<{}>", h)).collect::<Vec<_>>().join(", ");
                        println!("  {YELLOW}missing header(s) detected: {}{RESET}", missing_str, YELLOW=YELLOW, RESET=RESET);
                    }
                    if !missing_vars.is_empty() {
                        for (vname, decl) in &missing_vars {
                            println!("  {YELLOW}undeclared variable detected: '{vname}' (suggested: {decl}){RESET}", vname=vname, decl=decl, YELLOW=YELLOW, RESET=RESET);
                        }
                    }

                    let prompt = format!(
                        "  {YELLOW}apply auto-fixes and try again?{RESET} [Y/n]  ",
                        YELLOW=YELLOW, RESET=RESET
                    );
                    let apply_fix = if auto_confirm || quick_mode { true } else { confirm(&prompt) };
                    if apply_fix {
                        let customized_vars: Vec<String> = missing_vars
                            .iter()
                            .map(|(vname, decl)| {
                                if auto_confirm || quick_mode {
                                    decl.clone()
                                } else {
                                    prompt_variable_declaration(vname, decl)
                                }
                            })
                            .collect();

                        if let Some(ref mut sess) = session {
                            for h in missing_headers {
                                sess.extra_headers.push(h);
                            }
                            for decl in customized_vars {
                                sess.extra_vars.push(decl);
                            }
                        } else {
                            for h in missing_headers {
                                local_headers.push(h);
                            }
                            for decl in customized_vars {
                                local_vars.push(decl);
                            }
                        }
                        if !quick_mode {
                            println!("  {CYAN}re-building with fixes …{RESET}", CYAN=CYAN, RESET=RESET);
                        }
                        continue; // Retry compilation loop!
                    }
                }

                if let Some(ref mut sess) = session {
                    sess.statements.pop();
                }
                let _ = fs::remove_file(&src_path);
                return false;
            }
            Ok(_) => {
                if !quick_mode {
                    println!("  {GREEN}✓  compiled{RESET}\n", GREEN=GREEN, RESET=RESET);
                }
            }
        }

        // ── Run via Persistent Daemon OR Standalone Binary ──
        if is_so {
            if let Some(ref mut sess) = session {
                if let Some(ref daemon) = sess.daemon {
                    if !quick_mode {
                        println!("  {CYAN}running inside live daemon process (PID: {}) …{RESET}\n", daemon.pid, CYAN=CYAN, RESET=RESET);
                        println!("  {DIM}─── output ────────────────────────────{RESET}", DIM=DIM, RESET=RESET);
                    }
                    match daemon.execute_snippet(&target_bin) {
                        Ok(_res) => {
                            if !quick_mode {
                                println!("  {DIM}─── exit: 0 ──────────────────────────{RESET}\n", DIM=DIM, RESET=RESET);
                                println!("  {GREEN}✓  syscall executed under persistent PID {}{RESET}", daemon.pid, GREEN=GREEN, RESET=RESET);
                            }
                        }
                        Err(e) => {
                            eprintln!("  {RED}✗  daemon execution error: {}{RESET}", e, RED=RED, RESET=RESET);
                        }
                    }
                }
            }
            let _ = fs::remove_file(&src_path);
            let _ = fs::remove_file(&target_bin);
            println!();
            return true;
        }

        if !quick_mode {
            println!("  {CYAN}running …{RESET}\n", CYAN=CYAN, RESET=RESET);
            println!("  {DIM}─── output ────────────────────────────{RESET}", DIM=DIM, RESET=RESET);
        }

        let run_out = Command::new(&bin_path).output();

        match run_out {
            Err(e) => eprintln!("  {RED}✗  run error: {}{RESET}", e, RED=RED, RESET=RESET),
            Ok(out) => {
                let stdout_str = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr_str = String::from_utf8_lossy(&out.stderr);
                if !stdout_str.is_empty() {
                    for line in stdout_str.lines() {
                        println!("  {GREEN}│{RESET}  {}", line, GREEN=GREEN, RESET=RESET);
                    }
                }
                if !stderr_str.is_empty() {
                    for line in stderr_str.lines() {
                        eprintln!("  {RED}│{RESET}  {}", line, RED=RED, RESET=RESET);
                    }
                }
                let exit_code = out.status.code().unwrap_or(-1);
                if !quick_mode {
                    println!("  {DIM}─── exit: {} ──────────────────────────{RESET}\n",
                        exit_code, DIM=DIM, RESET=RESET);

                    if out.status.success() {
                        println!("  {GREEN}✓  syscall executed{RESET}", GREEN=GREEN, RESET=RESET);
                    } else {
                        eprintln!("  {RED}✗  process exited with code {}{RESET}", exit_code, RED=RED, RESET=RESET);
                    }
                }

                // ── Memory address detection ─────────────────────────────────
                if let Some(ret_val) = parse_return_value(&stdout_str) {
                    if looks_like_address(ret_val) {
                        println!();
                        println!(
                            "  {YELLOW}⚑  kernel returned 0x{:016x} ({}) — looks like a memory address!{RESET}",
                            ret_val as u64, ret_val, YELLOW=YELLOW, RESET=RESET
                        );
                        // -m flag forces inspect; otherwise prompt
                        let do_inspect = mem_inspect || auto_confirm || quick_mode || confirm(&format!(
                            "  {YELLOW}inspect memory at that address?{RESET} [Y/n]  ",
                            YELLOW=YELLOW, RESET=RESET
                        ));
                        if do_inspect {
                            let (headers, vars) = if let Some(ref sess) = session {
                                (&sess.extra_headers, &sess.extra_vars)
                            } else {
                                (&local_headers, &local_vars)
                            };
                            run_memory_inspector(
                                &stmt,
                                headers,
                                vars,
                                &compiler,
                            );
                        }
                    }
                }
            }
        }

        let _ = fs::remove_file(&src_path);
        let _ = fs::remove_file(&bin_path);
        println!();
        return true;
    }
}

