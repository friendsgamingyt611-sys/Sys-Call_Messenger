# SysCall Messenger (`scm`)

[![Linux](https://img.shields.io/badge/OS-Linux-blue.svg)](https://kernel.org)
[![Language](https://img.shields.io/badge/Language-Rust%20%7C%20C-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

> Execute raw Linux syscalls or standard `libc` functions directly from your terminal.  
> `scm` dynamically constructs an optimized C program, resolves headers via `man` pages, compiles with `gcc` or `clang`, executes the kernel boundary call, and presents detailed return value and memory inspection metrics.

---

## Key Features

- **Direct Kernel Execution**: Call any Linux syscall (`SYS_write`, `SYS_getpid`, etc.) or `libc` function directly from command line arguments or an interactive REPL.
- **Persistent Live Daemon Mode (`-R`)**: Keeps state, open file descriptors, allocated buffers, and process context active across commands within a single persistent OS process using background daemon sockets and dynamic `.so` compilation.
- **Interactive Code Space Management**: Inspect (`cs`), focus on specific execution points (`fc`), insert (`ins`), or replace/delete (`rpc`) statements dynamically in your workspace.
- **Focused Execution Control**: Focus evaluation on specific syscall statements while suppressing execution of statements below the focus point.
- **Built-in Terminal Code Editor (`-e`)**: Built-in full-screen TUI code editor with INSERT (`INS`) and OVERWRITE (`OVR`) modes, mouse scrolling, and line editing.
- **Automated Header & Variable Inference**: Detects missing `#include` directives and undeclared buffers (e.g., `char buf[4096]`), offering automatic fixes.
- **Memory Address Inspector (`-m`)**: Automatically detects when return values represent kernel or user space virtual addresses (`0x10000`–`0x7FFFFFFFFFFF`), inspecting VMA memory maps (`/proc/self/maps`), string representations, and formatted hex dumps safely via `sigsetjmp`/`siglongjmp`.

---

## Architecture Overview

```
User Input / REPL Command
          │
          ▼
 Auto-Header Discovery & Variable Inference
          │
          ▼
    C Code Space Generation
          │
   ┌──────┴─────────────────────────────────┐
   │                                        │
[Single-Shot Mode]             [Persistent Session Mode (-R)]
   │                                        │
Compile & Execute Binary        Compile snippet to .so shared object
   │                                        │
Print Kernel Return Value       Dispatch via UNIX socket to Daemon Worker
                                            │
                                Execute under fixed PID & return result
```

---

## Installation & Setup

### Prerequisites
- Linux kernel 4.x or newer
- Rust toolchain (`cargo`, `rustc`)
- C compiler (`gcc` or `clang`)

### Building & Installing

```bash
# Clone the repository
git clone https://github.com/example/syscall-messenger.git
cd syscall-messenger

# Build release binary
cargo build --release

# Install binary to ~/.cargo/bin (recommended)
cargo install --path .

# Or run using the launcher script
./run.sh [flags] '<call>'
```

---

## Quick Start & Usage

```bash
# Single-shot syscall execution
scm 'write(1, "Hello Kernel\n", 13)'
scm 'getpid()'
scm 'openat(AT_FDCWD, "src/main.rs", O_RDONLY)'

# Interactive persistent session (file descriptors & state persist)
scm -R

# Interactive REPL with auto-confirmation and memory inspector
scm -R -y -m
```

---

## CLI Command-Line Options

| Flag | Long Form | Description |
| :--- | :--- | :--- |
| `-R` | `--runner-session` | Enable **Persistent Session Mode**. File descriptors, variables, and state remain active in a background daemon process across commands. |
| `-q` | `--quick` | Enable **Quick Mode**. Suppresses C source code box previews and outputs kernel return values instantly. |
| `-e` | `--editor` | Open the compiled C code space in the **built-in terminal editor** before execution. `Ctrl+S` compiles and runs; `Esc` cancels. |
| `-y` | `--yes` | Auto-confirm all prompts (header fixes, compilation prompts, memory inspection). Ideal for scripting. |
| `-m` | `--mem-inspect` | Automatically trigger memory inspection whenever a pointer-like kernel return address is returned. |
| `-c` | `--compiler <bin>` | Specify compiler path (e.g., `-c /usr/bin/clang`). Defaults to searching `gcc` → `clang` → `cc`. |
| | `--tui` | Launch the split-pane TUI messenger dashboard. |
| `-h` | `--help` | Display usage options and version metadata. |

---

## Code Space & REPL Command Reference

In `scm`, **Code Space** represents your active C program workspace.

```
╔══════════════════════════════════════════════════════╗
║                   SysCall Messenger                  ║
╚══════════════════════════════════════════════════════╝
 [Persistent Runner Session (-R) Active — FDs, state, & code space persist]
 Type cs to view code space. Type cs run to execute code space.
 Type any function/syscall (e.g. openat(AT_FDCWD, "file.txt", O_RDONLY))
```

### REPL Commands

| Command | Usage | Description |
| :--- | :--- | :--- |
| `cs` | `cs` | Print the current C code space with line numbers and syntax highlighting. |
| `cs run` | `cs run` | Compile and execute the current C code space. |
| `fc` | `fc [L<num> \| num]` | Focus evaluation on line `L<num>` or statement `num`. Code past the focused statement is omitted from execution. |
| `ins` | `ins L<num> <code...>` | Insert a line of code at position `<num>` and shift following lines down. Use `NULL` for blank lines. |
| `rpc` | `rpc L<num> <code...>` | Replace line `<num>` with new code. Use `rpc L<num> NULL` or `del L<num>` to delete a line. |
| `rpc L*` | `rpc L*` | Erase all statements from the active code space. |
| `clr` | `clr` | Reset code space, clear terminal screen, and reset process state. |
| `session` | `session` | Display session diagnostics, open headers, and declared variables. |
| `exit` | `exit` / `quit` / `q` | Exit the REPL session. |

---

## Focused Execution Control (`fc`)

The `fc` command lets you focus kernel return value evaluation on a specific statement in your code space while suppressing statements that follow it:

1. **Preceding Statements**: Statements prior to the focused line execute sequentially so dependencies (such as buffer initialization or file openings) are maintained.
2. **Focused Statement**: The target statement's return value and `errno` are captured and displayed.
3. **Subsequent Statements**: Statements below the focus point are excluded from the C execution block.

```
scm> fc L11
  ✓  focused kernel return value on statement [2] (C line L11): openat(AT_FDCWD, "src/main.rs", O_RDONLY)
  save changes to code space? [Enter to save / q to discard]  
  ✓  code space updated (use 'cs run' to execute)

scm> cs run
  [kernel return value: 3]
```

---

## Memory Inspector (`-m`)

When a kernel call yields a pointer or memory address in user space (`0x10000` to `0x7FFFFFFFFFFF`), `scm` activates memory inspection:

```
⚑  kernel returned 0x00007ffef566c640 — looks like a memory address!
  ┌─── memory inspector ──────────────────────────────────────
  │  address  : 0x7ffef566c640
  │  hex      : 0x00007ffef566c640
  [maps region]  7ffef5659000-7ffef5679000 rw-p 00000000 00:00 0  [stack]
  │  type hint : null-terminated C string (37 bytes)
  │  value     : "/home/user/projects/Sys-Call_Messenger"
  │  hex dump  : (up to 64 bytes)

    0000: 2f 68 6f 6d 65 2f 75 73 65 72 2f 70 72 6f 6a 65  | /home/user/proje
    0010: 63 74 73 2f 53 79 73 2d 43 61 6c 6c 5f 4d 65 73  | cts/Sys-Call_Mes
    0020: 73 65 6e 67 65 72 00 00 00 00 00 00 00 00 00 00  | senger..........
    0030: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  | ................
  └───────────────────────────────────────────────────────────
```

---

## Built-in Terminal Editor (`-e`)

Launching with `-e` opens the generated C program in an interactive terminal editor before compilation.

| Keybinding | Function |
| :--- | :--- |
| `Ctrl+S` | Save code space modifications and immediately compile/run. |
| `Esc` / `Ctrl+Q` | Cancel editing session. |
| `i` | Toggle **INSERT** mode (bar cursor). |
| `o` | Toggle **OVERWRITE** mode (block cursor). |
| `Delete` | Remove current line. |
| `Ctrl+C` / `Ctrl+V` | Copy current line / Paste internal clipboard. |
| `Mouse Wheel` | Viewport scrolling. |

---

## Codebase Organization

```
src/
├── main.rs          # CLI entry point, flag parsing, REPL execution loop
├── compiler.rs      # C code generation, compiler dispatch, focus control, memory inspector
├── daemon.rs        # Background UNIX domain socket server & persistent process manager
├── editor.rs        # Built-in terminal TUI C source editor
├── messenger.rs     # IPC FIFO messaging protocol
├── syscall_layer.rs # Dynamic Linux syscall parser & signature layer
├── ui.rs            # Split-pane TUI dashboard UI
└── cli.rs           # Command & expression parser
```

---

## License

Distributed under the MIT License. See `LICENSE` for details.
