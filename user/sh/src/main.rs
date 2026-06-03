#![no_std]
#![no_main]
extern crate alloc;

use alloc::vec::Vec;
use libos::{write, read, open, close, getdents, spawn, exit, sys_yield, create, set_raw, waitpid, mkdir, unlink};

// ── ANSI helpers ──────────────────────────────────────────────────────────────
const RESET:  &[u8] = b"\x1b[0m";
const BOLD:   &[u8] = b"\x1b[1m";
const REV:    &[u8] = b"\x1b[7m";
const GREEN:  &[u8] = b"\x1b[32m";
const CYAN:   &[u8] = b"\x1b[36m";
const YELLOW: &[u8] = b"\x1b[33m";
const CLR:    &[u8] = b"\x1b[2J\x1b[H";

fn wrt(s: &[u8])   { write(1, s.as_ptr(), s.len()); }
fn wrt_str(s: &str) { wrt(s.as_bytes()); }
fn wrt_nl()         { wrt(b"\n"); }

fn wrt_num(mut n: usize) {
    if n == 0 { wrt(b"0"); return; }
    let mut tmp = [0u8; 20];
    let mut len = 0;
    while n > 0 { tmp[len] = b'0' + (n % 10) as u8; n /= 10; len += 1; }
    let mut out = [0u8; 20];
    for i in 0..len { out[i] = tmp[len - 1 - i]; }
    wrt(&out[..len]);
}

fn fmt_decimal(buf: &mut [u8], mut n: usize) -> usize {
    if n == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 20];
    let mut len = 0;
    while n > 0 { tmp[len] = b'0' + (n % 10) as u8; n /= 10; len += 1; }
    for i in 0..len { buf[i] = tmp[len - 1 - i]; }
    len
}

// Emit ESC[nD (move cursor left n columns)
fn move_left(n: usize) {
    if n == 0 { return; }
    let mut seq = [0u8; 8];
    seq[0] = 0x1b; seq[1] = b'[';
    let mut p = 2;
    p += fmt_decimal(&mut seq[p..], n);
    seq[p] = b'D'; p += 1;
    wrt(&seq[..p]);
}

// ── Shell ─────────────────────────────────────────────────────────────────────
struct Shell {
    cwd:     Vec<u8>,
    history: Vec<Vec<u8>>,
}

impl Shell {
    fn new() -> Self {
        Shell { cwd: b"/".to_vec(), history: Vec::new() }
    }

    fn prompt(&self) {
        wrt(GREEN); wrt(b"guest@openv"); wrt(RESET);
        wrt(b":"); wrt(CYAN);
        if self.cwd == b"/" { wrt(b"~"); } else { wrt(&self.cwd); }
        wrt(RESET); wrt(b"$ ");
    }

    // Raw-mode line editor: arrow keys, history, basic editing.
    // Caller must already have enabled raw mode.
    fn read_line_raw(&mut self) -> Vec<u8> {
        let hist_len = self.history.len();
        let mut buf: Vec<u8>  = Vec::new();
        let mut cursor: usize = 0;
        let mut hist_idx      = hist_len; // past-end = live input
        let mut saved: Vec<u8> = Vec::new(); // saved live input during history nav

        loop {
            let c = read_raw_byte();
            match c {
                b'\r' | b'\n' => {
                    wrt(b"\r\n");
                    return buf;
                }

                // ── Escape sequences (arrows, Home, End, Delete) ──────────────
                0x1b => {
                    let b1 = read_raw_byte();
                    if b1 != b'[' { continue; }
                    let b2 = read_raw_byte();
                    match b2 {
                        b'A' => { // Up — previous history entry
                            if hist_idx > 0 {
                                if hist_idx == hist_len { saved = buf.clone(); }
                                hist_idx -= 1;
                                let entry = self.history[hist_idx].clone();
                                Self::replace_input(&buf, cursor, &entry);
                                cursor = entry.len();
                                buf    = entry;
                            }
                        }
                        b'B' => { // Down — next history entry
                            if hist_idx < hist_len {
                                hist_idx += 1;
                                let entry = if hist_idx == hist_len {
                                    saved.clone()
                                } else {
                                    self.history[hist_idx].clone()
                                };
                                Self::replace_input(&buf, cursor, &entry);
                                cursor = entry.len();
                                buf    = entry;
                            }
                        }
                        b'C' => { // Right
                            if cursor < buf.len() {
                                wrt(&buf[cursor..cursor + 1]);
                                cursor += 1;
                            }
                        }
                        b'D' => { // Left
                            if cursor > 0 {
                                move_left(1);
                                cursor -= 1;
                            }
                        }
                        b'H' => { // Home
                            if cursor > 0 {
                                move_left(cursor);
                                cursor = 0;
                            }
                        }
                        b'F' => { // End
                            if cursor < buf.len() {
                                wrt(&buf[cursor..]);
                                cursor = buf.len();
                            }
                        }
                        b'3' => { // Delete key (ESC[3~)
                            read_raw_byte(); // consume '~'
                            if cursor < buf.len() {
                                buf.remove(cursor);
                                wrt(&buf[cursor..]);
                                wrt(b" ");
                                move_left(buf.len() - cursor + 1);
                            }
                        }
                        _ => {}
                    }
                }

                // ── Backspace ─────────────────────────────────────────────────
                0x7f | 0x08 => {
                    if cursor > 0 {
                        buf.remove(cursor - 1);
                        cursor -= 1;
                        move_left(1);
                        wrt(&buf[cursor..]);
                        wrt(b" ");
                        move_left(buf.len() - cursor + 1);
                    }
                }

                // ── Control keys ──────────────────────────────────────────────
                0x03 => { // Ctrl-C — cancel line
                    wrt(b"^C\r\n");
                    return Vec::new();
                }
                0x01 => { // Ctrl-A — beginning of line
                    if cursor > 0 { move_left(cursor); cursor = 0; }
                }
                0x05 => { // Ctrl-E — end of line
                    if cursor < buf.len() {
                        wrt(&buf[cursor..]);
                        cursor = buf.len();
                    }
                }
                0x15 => { // Ctrl-U — kill to start of line
                    move_left(cursor);
                    for _ in 0..buf.len() { wrt(b" "); }
                    move_left(buf.len());
                    buf.clear();
                    cursor = 0;
                }

                // ── Printable characters ──────────────────────────────────────
                c if c >= 0x20 => {
                    buf.insert(cursor, c);
                    cursor += 1;
                    wrt(&buf[cursor - 1..]); // print from insertion point to end
                    let tail = buf.len() - cursor;
                    if tail > 0 { move_left(tail); }
                }

                _ => {}
            }
        }
    }

    // Overwrite the current on-screen input with new_buf, erasing any leftover characters.
    fn replace_input(old_buf: &[u8], old_cursor: usize, new_buf: &[u8]) {
        move_left(old_cursor);
        wrt(new_buf);
        if old_buf.len() > new_buf.len() {
            let extra = old_buf.len() - new_buf.len();
            for _ in 0..extra { wrt(b" "); }
            move_left(extra);
        }
    }

    fn parse(line: &[u8]) -> Vec<Vec<u8>> {
        let mut args: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut in_q = false;
        let mut qch  = 0u8;
        for &b in line {
            if in_q {
                if b == qch { in_q = false; } else { cur.push(b); }
            } else if b == b'"' || b == b'\'' {
                in_q = true; qch = b;
            } else if b == b' ' || b == b'\t' {
                if !cur.is_empty() { args.push(cur.clone()); cur.clear(); }
            } else {
                cur.push(b);
            }
        }
        if !cur.is_empty() { args.push(cur); }
        args
    }

    fn resolve(&self, p: &[u8]) -> Vec<u8> {
        if p.starts_with(b"/") { return p.to_vec(); }
        let mut full = self.cwd.clone();
        if !full.ends_with(b"/") { full.push(b'/'); }
        full.extend_from_slice(p);
        full
    }

    fn execute(&mut self, args: &[Vec<u8>]) {
        let cmd = args[0].as_slice();
        match cmd {
            b"echo"                  => self.cmd_echo(&args[1..]),
            b"ls"                    => self.cmd_ls(&args[1..]),
            b"cat"                   => self.cmd_cat(&args[1..]),
            b"pwd"                   => { wrt(&self.cwd); wrt_nl(); }
            b"cd"                    => self.cmd_cd(&args[1..]),
            b"mkdir"                 => self.cmd_mkdir(&args[1..]),
            b"rm"                    => self.cmd_rm(&args[1..]),
            b"help" | b"?"           => self.cmd_help(),
            b"clear"                 => wrt(CLR),
            b"history" | b".history" => self.cmd_history(),
            b"nano" => {
                // Nano manages raw mode internally; restore ours after it exits.
                set_raw(0);
                self.cmd_nano(&args[1..]);
                set_raw(1);
            }
            b"whoami"             => wrt(b"guest\n"),
            b"hostname"           => wrt(b"openv\n"),
            b"uname" | b"uname-a" => wrt(b"openv 0.1.0 riscv64gc-unknown-none-elf\n"),
            b"exit" | b"quit"     => { set_raw(0); exit(0); }
            _ => {
                // External binary: drop raw mode so child gets cooked console,
                // then restore it after the child exits. This also ensures a
                // failed spawn never leaves the shell in a broken input state.
                let mut path: Vec<u8> = Vec::new();
                if !cmd.starts_with(b"/") { path.push(b'/'); }
                path.extend_from_slice(cmd);

                set_raw(0);
                let pid = spawn(path.as_ptr(), path.len());
                if pid < 0 {
                    wrt(b"sh: ");
                    wrt(cmd);
                    wrt(b": command not found\n");
                } else {
                    let mut status: i32 = 0;
                    waitpid(pid, &mut status as *mut i32);
                }
                set_raw(1); // always restore — even after a failed spawn
            }
        }
    }

    // ── Builtins ──────────────────────────────────────────────────────────────

    fn cmd_echo(&self, args: &[Vec<u8>]) {
        let mut first = true;
        for arg in args { if !first { wrt(b" "); } first = false; wrt(arg); }
        wrt_nl();
    }

    fn cmd_ls(&self, args: &[Vec<u8>]) {
        let path = if args.is_empty() { self.cwd.clone() } else { self.resolve(&args[0]) };
        let mut buf = [0u8; 4096];
        let n = getdents(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len());
        if n < 0 {
            wrt(b"ls: cannot access '"); wrt(&path); wrt(b"': No such file or directory\n");
            return;
        }
        let mut pos = 0usize;
        while pos < n as usize {
            let start = pos;
            while pos < n as usize && buf[pos] != 0 { pos += 1; }
            if pos > start { wrt(CYAN); wrt(&buf[start..pos]); wrt(RESET); wrt(b"\n"); }
            pos += 1;
        }
    }

    fn cmd_cat(&self, args: &[Vec<u8>]) {
        if args.is_empty() { wrt(b"cat: missing file argument\n"); return; }
        for arg in args {
            let path = self.resolve(arg);
            let fd = open(path.as_ptr(), path.len(), 0);
            if fd < 0 {
                wrt(b"cat: "); wrt(&path); wrt(b": No such file or directory\n");
                continue;
            }
            let mut buf = [0u8; 1024];
            loop {
                let n = read(fd as usize, buf.as_mut_ptr(), buf.len());
                if n <= 0 { break; }
                wrt(&buf[..n as usize]);
            }
            close(fd);
        }
    }

    fn cmd_cd(&mut self, args: &[Vec<u8>]) {
        let path = if args.is_empty() { b"/".to_vec() } else { self.resolve(&args[0]) };
        let mut buf = [0u8; 16];
        let n = getdents(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len());
        if n < 0 {
            wrt(b"cd: "); wrt(&path); wrt(b": No such file or directory\n");
        } else {
            self.cwd = path;
        }
    }

    fn cmd_mkdir(&self, args: &[Vec<u8>]) {
        if args.is_empty() { wrt(b"mkdir: missing operand\n"); return; }
        for arg in args {
            let path = self.resolve(arg);
            if mkdir(&path) != 0 {
                wrt(b"mkdir: cannot create directory '"); wrt(&path); wrt(b"'\n");
            }
        }
    }

    fn cmd_rm(&self, args: &[Vec<u8>]) {
        if args.is_empty() { wrt(b"rm: missing operand\n"); return; }
        for arg in args {
            let path = self.resolve(arg);
            if unlink(&path) != 0 {
                wrt(b"rm: cannot remove '"); wrt(&path); wrt(b"': No such file or directory\n");
            }
        }
    }

    fn cmd_history(&self) {
        if self.history.is_empty() {
            wrt(b"(no history)\n");
            return;
        }
        for (i, entry) in self.history.iter().enumerate() {
            // Right-align the index number in a 4-wide field
            let idx = i + 1;
            if idx < 10   { wrt(b"   "); }
            else if idx < 100  { wrt(b"  "); }
            else if idx < 1000 { wrt(b" "); }
            wrt_num(idx);
            wrt(b"  ");
            wrt(entry);
            wrt_nl();
        }
    }

    fn cmd_help(&self) {
        wrt(BOLD); wrt(b"openv shell builtins:\n"); wrt(RESET);
        let cmds = [
            ("echo [args]",   "Print arguments to stdout"),
            ("ls [path]",     "List directory contents"),
            ("cat <file>",    "Print file to stdout"),
            ("pwd",           "Print working directory"),
            ("cd [path]",     "Change directory (default /)"),
            ("mkdir <dir>",   "Create directory"),
            ("rm <file>",     "Remove file or directory"),
            ("nano <file>",   "Text editor (^O save, ^X exit)"),
            ("history",       "Show command history"),
            ("clear",         "Clear the screen"),
            ("whoami",        "Print current user"),
            ("hostname",      "Print hostname"),
            ("uname",         "Print system info"),
            ("exit",          "Exit the shell"),
        ];
        wrt(b"  Editing: Up/Down=history  Left/Right=move  Ctrl-A/E=start/end  Ctrl-U=kill\n\n");
        for (cmd, desc) in &cmds {
            wrt(b"  "); wrt(CYAN); wrt_str(cmd); wrt(RESET);
            let pad = 18usize.saturating_sub(cmd.len());
            for _ in 0..pad { wrt(b" "); }
            wrt_str(desc); wrt_nl();
        }
    }

    fn cmd_nano(&self, args: &[Vec<u8>]) {
        if args.is_empty() { wrt(b"nano: missing filename\n"); return; }
        let path = self.resolve(&args[0]);
        NanoEditor::new(path).run();
    }

    fn run(&mut self) {
        set_raw(1); // raw mode for the entire shell session
        loop {
            self.prompt();
            let line = self.read_line_raw();
            if line.is_empty() { continue; }
            let args = Self::parse(&line);
            if args.is_empty() { continue; }
            // Append to history, skipping consecutive duplicates
            if self.history.last().map(|e| e.as_slice()) != Some(line.as_slice()) {
                self.history.push(line);
            }
            // args is already parsed and owned; execute uses it directly
            self.execute(&args);
        }
    }
}

// ── Nano editor ───────────────────────────────────────────────────────────────

const TERM_ROWS:    usize = 24;
const TERM_COLS:    usize = 80;
const CONTENT_ROWS: usize = TERM_ROWS - 3;

fn read_raw_byte() -> u8 {
    let mut c = 0u8;
    loop {
        let n = read(0, &mut c as *mut u8, 1);
        if n > 0 { return c; }
        sys_yield();
    }
}

fn read_key() -> NanoKey {
    let c = read_raw_byte();
    match c {
        0x1b => {
            let b1 = read_raw_byte();
            if b1 != b'[' { return NanoKey::Other; }
            let b2 = read_raw_byte();
            match b2 {
                b'A' => NanoKey::Up,
                b'B' => NanoKey::Down,
                b'C' => NanoKey::Right,
                b'D' => NanoKey::Left,
                b'H' => NanoKey::Home,
                b'F' => NanoKey::End,
                b'3' => { read_raw_byte(); NanoKey::Delete }
                b'5' => { read_raw_byte(); NanoKey::PageUp }
                b'6' => { read_raw_byte(); NanoKey::PageDown }
                _ => NanoKey::Other,
            }
        }
        0x0f => NanoKey::CtrlO,
        0x18 => NanoKey::CtrlX,
        0x0b => NanoKey::CtrlK,
        0x7f | 0x08 => NanoKey::Backspace,
        b'\r' | b'\n' => NanoKey::Enter,
        c if c >= 0x20 => NanoKey::Char(c),
        _ => NanoKey::Other,
    }
}

#[derive(Debug)]
enum NanoKey {
    Char(u8),
    Enter, Backspace, Delete,
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown,
    CtrlO, CtrlX, CtrlK,
    Other,
}

fn move_cursor(row: usize, col: usize) {
    let mut buf = [0u8; 16];
    buf[0] = 0x1b; buf[1] = b'[';
    let mut p = 2;
    p += fmt_decimal(&mut buf[p..], row);
    buf[p] = b';'; p += 1;
    p += fmt_decimal(&mut buf[p..], col);
    buf[p] = b'H'; p += 1;
    wrt(&buf[..p]);
}

fn pad_line(used: usize) {
    let n = TERM_COLS.saturating_sub(used);
    for _ in 0..n { wrt(b" "); }
    wrt(RESET);
}

struct NanoEditor {
    filename: Vec<u8>,
    lines:    Vec<Vec<u8>>,
    row:      usize,
    col:      usize,
    scroll:   usize,
    modified: bool,
    msg:      Vec<u8>,
}

impl NanoEditor {
    fn new(filename: Vec<u8>) -> Self {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let fd = open(filename.as_ptr(), filename.len(), 0);
        if fd >= 0 {
            let mut content: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = read(fd as usize, buf.as_mut_ptr(), buf.len());
                if n <= 0 { break; }
                content.extend_from_slice(&buf[..n as usize]);
            }
            close(fd);
            let mut start = 0;
            for i in 0..content.len() {
                if content[i] == b'\n' {
                    lines.push(content[start..i].to_vec());
                    start = i + 1;
                }
            }
            if start < content.len() { lines.push(content[start..].to_vec()); }
        }
        if lines.is_empty() { lines.push(Vec::new()); }
        NanoEditor { filename, lines, row: 0, col: 0, scroll: 0, modified: false, msg: Vec::new() }
    }

    fn render(&self) {
        wrt(b"\x1b[?25l");
        wrt(CLR);
        move_cursor(1, 1);
        wrt(REV); wrt(BOLD);
        wrt(b"  openv nano 0.1   ");
        wrt(&self.filename);
        if self.modified { wrt(b"  [Modified]"); }
        pad_line(19 + self.filename.len() + if self.modified { 12 } else { 0 });

        for i in 0..CONTENT_ROWS {
            move_cursor(i + 2, 1);
            wrt(b"\x1b[K");
            let li = self.scroll + i;
            if li < self.lines.len() {
                let line = &self.lines[li];
                wrt(&line[..line.len().min(TERM_COLS)]);
            }
        }

        move_cursor(TERM_ROWS - 1, 1);
        wrt(b"\x1b[K");
        if !self.msg.is_empty() {
            wrt(YELLOW); wrt(&self.msg); wrt(RESET);
        } else {
            wrt(b"Ln "); wrt_num(self.row + 1);
            wrt(b", Col "); wrt_num(self.col + 1);
        }

        move_cursor(TERM_ROWS, 1);
        wrt(REV);
        wrt(b"  ^O Write   ^X Exit   ^K Del Line   Arrows: move   Home/End");
        pad_line(61);

        let cur_row = self.row - self.scroll + 2;
        move_cursor(cur_row, self.col + 1);
        wrt(b"\x1b[?25h");
    }

    fn adjust_scroll(&mut self) {
        if self.row < self.scroll { self.scroll = self.row; }
        else if self.row >= self.scroll + CONTENT_ROWS {
            self.scroll = self.row + 1 - CONTENT_ROWS;
        }
    }

    fn clamp_col(&mut self) {
        let len = self.lines[self.row].len();
        if self.col > len { self.col = len; }
    }

    fn save(&mut self) {
        let fd = create(&self.filename);
        if fd < 0 { self.msg = b"Error: cannot write file!".to_vec(); return; }
        for (i, line) in self.lines.iter().enumerate() {
            write(fd as usize, line.as_ptr(), line.len());
            if i + 1 < self.lines.len() { write(fd as usize, b"\n".as_ptr(), 1); }
        }
        write(fd as usize, b"\n".as_ptr(), 1);
        close(fd);
        self.modified = false;
        self.msg = b"File written.".to_vec();
    }

    fn handle(&mut self, key: NanoKey) -> bool {
        self.msg.clear();
        match key {
            NanoKey::CtrlX => return false,
            NanoKey::CtrlO => self.save(),
            NanoKey::CtrlK => {
                self.lines.remove(self.row);
                if self.lines.is_empty() { self.lines.push(Vec::new()); }
                if self.row >= self.lines.len() { self.row = self.lines.len() - 1; }
                self.clamp_col(); self.modified = true;
            }
            NanoKey::Up => {
                if self.row > 0 { self.row -= 1; self.clamp_col(); self.adjust_scroll(); }
            }
            NanoKey::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1; self.clamp_col(); self.adjust_scroll();
                }
            }
            NanoKey::Left => {
                if self.col > 0 { self.col -= 1; }
                else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                    self.adjust_scroll();
                }
            }
            NanoKey::Right => {
                let len = self.lines[self.row].len();
                if self.col < len { self.col += 1; }
                else if self.row + 1 < self.lines.len() {
                    self.row += 1; self.col = 0; self.adjust_scroll();
                }
            }
            NanoKey::Home     => { self.col = 0; }
            NanoKey::End      => { self.col = self.lines[self.row].len(); }
            NanoKey::PageUp   => {
                self.row = self.row.saturating_sub(CONTENT_ROWS);
                self.clamp_col(); self.adjust_scroll();
            }
            NanoKey::PageDown => {
                self.row = (self.row + CONTENT_ROWS).min(self.lines.len() - 1);
                self.clamp_col(); self.adjust_scroll();
            }
            NanoKey::Enter => {
                let rest = self.lines[self.row].split_off(self.col);
                self.row += 1;
                self.lines.insert(self.row, rest);
                self.col = 0; self.adjust_scroll(); self.modified = true;
            }
            NanoKey::Backspace => {
                if self.col > 0 {
                    self.col -= 1;
                    self.lines[self.row].remove(self.col);
                    self.modified = true;
                } else if self.row > 0 {
                    let cur = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                    self.lines[self.row].extend_from_slice(&cur);
                    self.adjust_scroll(); self.modified = true;
                }
            }
            NanoKey::Delete => {
                let len = self.lines[self.row].len();
                if self.col < len {
                    self.lines[self.row].remove(self.col); self.modified = true;
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].extend_from_slice(&next); self.modified = true;
                }
            }
            NanoKey::Char(c) => {
                self.lines[self.row].insert(self.col, c);
                self.col += 1; self.modified = true;
            }
            NanoKey::Other => {}
        }
        true
    }

    fn run(&mut self) {
        set_raw(1);
        loop {
            self.render();
            let key = read_key();
            if !self.handle(key) { break; }
        }
        set_raw(0);
        wrt(CLR);
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────
#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    Shell::new().run();
    0
}
