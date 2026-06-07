# 10. TTY / Line Discipline

The TTY system handles keyboard input and terminal semantics.

### Global State

```rust
static LINE_DISC_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ECHO_ENABLED: AtomicBool = AtomicBool::new(true);
static RAW_MODE:     AtomicBool = AtomicBool::new(false);
```

### Cooked Mode (default)

When `RAW_MODE = false`, `sys_read` on the console:

```
loop:
    ch = uart::try_get_char()   // non-blocking UART RX
    if ch is None: yield CPU and retry

    match ch:
        '\n' | '\r':
            push '\n' to LINE_DISC_BUFFER
            if ECHO_ENABLED: uart::put_char('\n')
            return LINE_DISC_BUFFER contents to caller; clear buffer

        '\x08' | '\x7F':        // Backspace or DEL
            if buffer not empty:
                buffer.pop()
                if ECHO_ENABLED: uart::write("\x08 \x08")  // erase on terminal

        '\x03':                  // Ctrl-C
            kill current foreground process with exit(130)
            clear LINE_DISC_BUFFER

        other:
            push to LINE_DISC_BUFFER
            if ECHO_ENABLED: uart::put_char(ch)
```

### Raw Mode

When `RAW_MODE = true`, `sys_read` polls `uart::try_get_char()` in a loop (yielding between attempts) and returns as soon as one character is received, without echo or buffering.

The shell uses raw mode for:
- Reading arrow keys (escape sequences) for history navigation.
- The built-in `nano` editor.

### Echo Control

`sys_set_echo(false)` is called during password prompts to suppress character echo. The shell restores echo with `sys_set_echo(true)` after reading the password.

---
[Back to Index](README.md)
