# SysCall Messenger (`scm`)

> Make any Linux syscall or libc function call directly from your terminal.  
> `scm` builds a real C program, compiles it with `gcc`/`clang`, and shows you exactly what the kernel returns.

---

## How it works

1. You type a C function call (e.g. `getcwd(buf, sizeof(buf))`)
2. `scm` queries `man` pages to auto-discover required `#include` headers
3. It wraps your call in a minimal C template, compiles it, and runs it
4. The kernel's return value is printed
5. If the return value looks like a memory address, `scm` can inspect what's there

```
your input → C template → gcc → kernel → return value → (optional) memory inspector
```

---

## Installation / Running

```bash
# Build
cargo build --release

# Run directly
./run.sh [flags] '<call>'

# Or use the compiled binary
./target/release/scm [flags] '<call>'

# Install globally (optional)
cargo install --path .
```

> `run.sh` always builds the latest binary before launching, safe to use during development.

---

## Usage

```
scm [flags] '<call>'     single-shot mode
scm [flags]              interactive REPL (type calls one by one)
```

---

## Flags

| Flag | Long form | Description |
|------|-----------|-------------|
| `-R` | `--runner-session` | Launch interactive REPL in **persistent session mode**. File descriptors, variable declarations, and state persist across commands. |
| `-e` | `--editor` | Open the generated C source in the **built-in editor** before compiling. `Ctrl+S` saves and runs. `Esc` cancels. |
| `-y` | `--yes` | Auto-confirm **all** prompts (compile, header fixes, memory inspect). Useful for scripting. |
| `-m` | `--mem-inspect` | Always run the **memory inspector** when the kernel returns a pointer-like value — no prompt needed. |
| `-c <bin>` | `--compiler <bin>` | Use a specific C compiler binary. Default search order: `gcc → clang → cc`. |
| `--tui` | | Launch the split-pane **TUI messenger** interface. |
| `-h` | `--help` | Print help text and exit. |

Flags can be combined freely:

```bash
scm -R                                 # interactive persistent session
scm -R -y                              # persistent session + auto-confirm fixes
scm -e -m 'getcwd(buf, sizeof(buf))'   # edit C, then auto-inspect pointer
scm -y -m 'mmap(0,4096,3,34,-1,0)'    # skip all prompts, inspect result
```

---

## Examples

```bash
# Basic libc wrappers
scm 'write(1, "hello\n", 6)'
scm 'getpid()'
scm 'getuid()'

# File operations
scm 'openat(AT_FDCWD, "file.txt", O_RDONLY)'
scm 'getcwd(buf, sizeof(buf))'
scm 'mkdir("/tmp/testdir", 0755)'

# Network
scm 'socket(AF_INET, SOCK_STREAM, 0)'

# Raw syscall numbers (via syscall(2))
scm '(SYS_write, 1, "hello\n", 6)'
scm '(SYS_getpid)'

# Memory inspection (pointer return value)
scm -m 'getcwd(buf, sizeof(buf))'

# Edit the C before running
scm -e 'openat(AT_FDCWD, "notes.txt", O_RDONLY)'

# Use clang instead of gcc
scm -c clang 'write(1, "hi\n", 3)'

# Skip all prompts (useful in scripts)
scm -y 'socket(AF_INET, SOCK_STREAM, 0)'
```

---

## Code Space & Interactive REPL Commands

In `scm`, **Code Space** is where your C program code lives:
- **Normal mode:** Code space is unique for every syscall command.
- **Session mode (`-R`):** Code space persists and accumulates across calls.

### Code Space REPL Commands

| Command | Description |
|---------|-------------|
| `cs` | Instantly print the current C code space (with line numbers & syntax highlighting) |
| `fc` | Focus kernel return value & errno evaluation on a specific line/syscall (`fc`, `fc L11`, `fc 1`) |
| `ins L<num> NULL` | Insert a blank line at position `<num>` and shift below lines down |
| `ins L<num> <code...>` | Insert a new code line at position `<num>` and shift below lines down |
| `rpc L<num> <code...>` | Replace line `<num>` in code space (e.g. `rpc L12 read(fd, buf, 100)`) |
| `rpc L<num> NULL` | Delete line `<num>` from code space (also accepts `del` or `delete`) |
| `rpc L*` | Clear all lines from code space (also accepts `rpc L *`) |
| `clr` | Clear code space and erase all memory, state, and variables |
| `session` | View current code space state, headers, variables, and statements |
| `exit` / `quit` / `q` | Exit REPL |

---

## Interactive REPL

Run `scm` with no arguments to enter the REPL (or `scm -R` for persistent session mode):

```
╔══════════════════════════════════════════════════════╗
║                   SysCall Messenger                  ║
╚══════════════════════════════════════════════════════╝
 [Persistent Runner Session (-R) Active — FDs, state, & code space persist]
 Type clr to clear code space. Type session to view state.
 Type rpc L<num> <code...> to edit line, rpc L<num> NULL to delete line, or rpc L* to erase all lines.

scm> openat(AT_FDCWD, "src/main.rs", O_RDONLY)
scm> read(3, buf, sizeof(buf) - 1)
scm> rpc L12 NULL                                # Deletes line 12 from code space
scm> rpc L*                                      # Erases all lines from code space
scm> clr                                         # Clears code space & state completely
scm> exit
```

---

## C Template


Every call is wrapped in this template:

```c
#include <unistd.h>
#include <sys/syscall.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
/* headers auto-discovered from man pages */

int main(void)
{
    /* variable declarations auto-added if needed */
    errno = 0;
    long res = (long)( <your call> );
    if (res < 0 || errno != 0) {
        printf("\n[kernel return value: %ld (errno %d: %s)]\n",
               res, errno, strerror(errno));
    } else {
        printf("\n[kernel return value: %ld]\n", res);
    }
    return 0;
}
```

### Auto-fixes

- **Missing headers** — if compilation fails because a type or constant is undefined, `scm` checks compiler errors and `man` pages and offers to add the missing `#include`.
- **Undeclared variables** — if you use `buf`, `st`, `fd`, etc. without declaring them, `scm` infers a sensible declaration (e.g. `char buf[1024] = {0}`) and offers to insert it automatically.

---

## Memory Inspector (`-m`)

When the kernel return value is in the user-space address range (`0x10000 – 0x7FFFFFFFFFFF`), `scm` recognises it as a virtual memory pointer.

**With `-m` flag:** inspector runs automatically (no prompt).  
**Without `-m` flag:** `scm` asks `inspect memory at that address? [Y/n]`.

The inspector compiles a second C program that:

| What | How |
|------|-----|
| Shows address in hex | `0x00007ffd...` |
| Finds the memory region | reads `/proc/self/maps` → prints VMA name (e.g. `[stack]`, `[heap]`, library) |
| String detection | `strnlen` up to 4096 bytes + printability check → prints string if clean |
| 64-byte hex dump | formatted with ASCII panel (like `xxd`) |
| Safe reads | uses `sigsetjmp`/`siglongjmp` to catch `SIGSEGV`/`SIGBUS` — never crashes |

Example output for `getcwd(buf, sizeof(buf))`:

```
⚑  kernel returned 0x00007ffef566c640 — looks like a memory address!
  ┌─── memory inspector ──────────────────────────────────────
  │  address  : 0x7ffef566c640
  │  hex      : 0x00007ffef566c640
  [maps region]  7ffef5659000-7ffef5679000 rw-p 00000000 00:00 0  [stack]
  │  type hint : null-terminated C string (42 bytes)
  │  value     : "/home/devansh/Documents/Sys-Call_Messenger"
  │  hex dump  : (up to 64 bytes)

    0000: 2f 68 6f 6d 65 2f 64 65 76 61 6e 73 68 2f 44 6f  | /home/devansh/Do
    0010: 63 75 6d 65 6e 74 73 2f 53 79 73 2d 43 61 6c 6c  | cuments/Sys-Call
    0020: 5f 4d 65 73 73 65 6e 67 65 72 00 00 00 00 00 00  | _Messenger......
    0030: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  | ................
  └───────────────────────────────────────────────────────────
```

---

## Built-in Editor (`-e`)

The `-e` flag opens the generated C source in a full terminal editor before compiling.

| Key | Action |
|-----|--------|
| `Ctrl+S` | Save and run immediately (no second prompt) |
| `Esc` / `Ctrl+Q` | Cancel editing |
| `i` | Switch to **INSERT** mode (blinking bar cursor) |
| `o` | Switch to **OVERWRITE** mode (block cursor, default) |
| `Delete` | Delete the entire current line |
| `Ctrl+C` | Copy current line to internal clipboard |
| `Ctrl+V` | Paste clipboard line(s) below cursor |
| `Tab` | Insert 4 spaces |
| `PageUp` / `PageDown` | Scroll by page |
| Mouse wheel | Scroll viewport (terminal emulator scrollback unaffected) |
| `Home` / `End` | Jump to start/end of line |

The current line is highlighted. Mode (INS/OVR) is shown in the header and status bar, with colour changing per mode (green = INS, blue/amber = OVR).

---

## Supported Syscalls

All ~450+ Linux syscalls are supported — `scm` uses `syscall(2)` or the libc wrapper directly. Any call that compiles with `gcc` on your kernel will work.

```bash
scm 'read(0, buf, 1)'
scm 'lseek(fd, 0, SEEK_END)'
scm 'mmap(0, 4096, PROT_READ|PROT_WRITE, MAP_ANONYMOUS|MAP_PRIVATE, -1, 0)'
scm 'fork()'
scm 'ptrace(PTRACE_TRACEME, 0, 0, 0)'
scm 'bpf(BPF_PROG_LOAD, 0, 0)'
```

---

## Project Structure

```
src/
  main.rs          — CLI flag parsing, REPL loop
  compiler.rs      — C template builder, compiler, runner, memory inspector
  editor.rs        — Built-in terminal C code editor (INS/OVR, mouse scroll)
  messenger.rs     — FIFO IPC channel (used by --tui)
  ui.rs            — Split-pane TUI interface
  syscall_layer.rs — Low-level syscall expression layer
  cli.rs           — Expression parser/executor
run.sh             — Build-and-run launcher script
```
