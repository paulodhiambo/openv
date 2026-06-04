#![no_std]
#![no_main]
extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use libos::{
    chdir, close, create, dup, dup2, exec_args, exit, fork, getdents, mkdir, open, pipe, read,
    set_raw, set_fg_pid, sys_yield, unlink, waitpid, write,
};

const RESET: &[u8] = b"\x1b[0m";
const BOLD: &[u8] = b"\x1b[1m";
const REV: &[u8] = b"\x1b[7m";
const GREEN: &[u8] = b"\x1b[32m";
const CYAN: &[u8] = b"\x1b[36m";
const YELLOW: &[u8] = b"\x1b[33m";
const CLR: &[u8] = b"\x1b[0m\x1b[3J\x1b[2J\x1b[H";

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}
fn wrt_str(s: &str) {
    wrt(s.as_bytes());
}
fn wrt_nl() {
    wrt(b"\n");
}

fn wrt_num(mut n: usize) {
    if n == 0 {
        wrt(b"0");
        return;
    }
    let mut tmp = [0u8; 20];
    let mut len = 0;
    while n > 0 {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    let mut out = [0u8; 20];
    for i in 0..len {
        out[i] = tmp[len - 1 - i];
    }
    wrt(&out[..len]);
}

fn fmt_decimal(buf: &mut [u8], mut n: usize) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut len = 0;
    while n > 0 {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        buf[i] = tmp[len - 1 - i];
    }
    len
}

fn move_left(n: usize) {
    if n == 0 {
        return;
    }
    let mut seq = [0u8; 8];
    seq[0] = 0x1b;
    seq[1] = b'[';
    let mut p = 2;
    p += fmt_decimal(&mut seq[p..], n);
    seq[p] = b'D';
    p += 1;
    wrt(&seq[..p]);
}

// ── Pipeline stage ────────────────────────────────────────────────────────────

struct Stage {
    args: Vec<Vec<u8>>,
    stdin_file: Option<Vec<u8>>,
    stdout_file: Option<Vec<u8>>,
}

// ── Shell ─────────────────────────────────────────────────────────────────────

struct Shell {
    cwd: Vec<u8>,
    history: Vec<Vec<u8>>,
    env: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Shell {
    fn new() -> Self {
        let env: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"HOME".to_vec(),     b"/".to_vec()),
            (b"USER".to_vec(),     b"guest".to_vec()),
            (b"SHELL".to_vec(),    b"/sh".to_vec()),
            (b"PATH".to_vec(),     b"/".to_vec()),
            (b"HOSTNAME".to_vec(), b"openv".to_vec()),
            (b"PWD".to_vec(),      b"/".to_vec()),
        ];
        Shell {
            cwd: b"/".to_vec(),
            history: Vec::new(),
            env,
        }
    }

    // ── Environment variable helpers ──────────────────────────────────────────

    fn env_get<'a>(&'a self, name: &[u8]) -> Option<&'a [u8]> {
        self.env.iter().find(|(k, _)| k.as_slice() == name).map(|(_, v)| v.as_slice())
    }

    fn env_set(&mut self, name: Vec<u8>, value: Vec<u8>) {
        if let Some(e) = self.env.iter_mut().find(|(k, _)| k.as_slice() == name.as_slice()) {
            e.1 = value;
        } else {
            self.env.push((name, value));
        }
    }

    fn env_unset(&mut self, name: &[u8]) {
        self.env.retain(|(k, _)| k.as_slice() != name);
    }

    // Expand `$VAR` and `${VAR}` in `line`; single-quoted spans are left as-is.
    fn expand_vars(line: &[u8], env: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        let mut in_single = false;
        while i < line.len() {
            let b = line[i];
            if in_single {
                if b == b'\'' { in_single = false; } else { out.push(b); }
                i += 1;
                continue;
            }
            match b {
                b'\'' => { in_single = true; i += 1; }
                b'$' if i + 1 < line.len() => {
                    i += 1;
                    if line[i] == b'{' {
                        i += 1;
                        let start = i;
                        while i < line.len() && line[i] != b'}' { i += 1; }
                        let name = &line[start..i];
                        if i < line.len() { i += 1; }
                        if let Some((_, v)) = env.iter().find(|(k, _)| k.as_slice() == name) {
                            out.extend_from_slice(v);
                        }
                    } else if line[i].is_ascii_alphabetic() || line[i] == b'_' {
                        let start = i;
                        while i < line.len() && (line[i].is_ascii_alphanumeric() || line[i] == b'_') {
                            i += 1;
                        }
                        let name = &line[start..i];
                        if let Some((_, v)) = env.iter().find(|(k, _)| k.as_slice() == name) {
                            out.extend_from_slice(v);
                        }
                        // undefined vars expand to empty (POSIX)
                    } else {
                        out.push(b'$');
                    }
                }
                _ => { out.push(b); i += 1; }
            }
        }
        out
    }

    // Detect a bare `NAME=value` line (no spaces in name).
    fn parse_assignment(line: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        let eq = line.iter().position(|&b| b == b'=')?;
        let name = &line[..eq];
        if name.is_empty() { return None; }
        let first = name[0];
        if !first.is_ascii_alphabetic() && first != b'_' { return None; }
        if !name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_') { return None; }
        Some((name.to_vec(), line[eq + 1..].to_vec()))
    }

    fn prompt(&self) {
        wrt(GREEN);
        wrt(b"guest@openv");
        wrt(RESET);
        wrt(b":");
        wrt(CYAN);
        if self.cwd == b"/" {
            wrt(b"~");
        } else {
            wrt(&self.cwd);
        }
        wrt(RESET);
        wrt(b"$ ");
    }

    fn read_line_raw(&mut self) -> Vec<u8> {
        let hist_len = self.history.len();
        let mut buf: Vec<u8> = Vec::new();
        let mut cursor: usize = 0;
        let mut hist_idx = hist_len;
        let mut saved: Vec<u8> = Vec::new();

        loop {
            let c = read_raw_byte();
            match c {
                b'\r' | b'\n' => {
                    wrt(b"\r\n");
                    return buf;
                }

                0x1b => {
                    let b1 = read_raw_byte();
                    if b1 != b'[' {
                        continue;
                    }
                    let b2 = read_raw_byte();
                    match b2 {
                        b'A' => {
                            if hist_idx > 0 {
                                if hist_idx == hist_len {
                                    saved = buf.clone();
                                }
                                hist_idx -= 1;
                                let entry = self.history[hist_idx].clone();
                                Self::replace_input(&buf, cursor, &entry);
                                cursor = entry.len();
                                buf = entry;
                            }
                        }
                        b'B' => {
                            if hist_idx < hist_len {
                                hist_idx += 1;
                                let entry = if hist_idx == hist_len {
                                    saved.clone()
                                } else {
                                    self.history[hist_idx].clone()
                                };
                                Self::replace_input(&buf, cursor, &entry);
                                cursor = entry.len();
                                buf = entry;
                            }
                        }
                        b'C' => {
                            if cursor < buf.len() {
                                wrt(&buf[cursor..cursor + 1]);
                                cursor += 1;
                            }
                        }
                        b'D' => {
                            if cursor > 0 {
                                move_left(1);
                                cursor -= 1;
                            }
                        }
                        b'H' => {
                            if cursor > 0 {
                                move_left(cursor);
                                cursor = 0;
                            }
                        }
                        b'F' => {
                            if cursor < buf.len() {
                                wrt(&buf[cursor..]);
                                cursor = buf.len();
                            }
                        }
                        b'3' => {
                            read_raw_byte();
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

                0x03 => {
                    wrt(b"^C\r\n");
                    return Vec::new();
                }
                0x01 => {
                    if cursor > 0 {
                        move_left(cursor);
                        cursor = 0;
                    }
                }
                0x05 => {
                    if cursor < buf.len() {
                        wrt(&buf[cursor..]);
                        cursor = buf.len();
                    }
                }
                0x15 => {
                    move_left(cursor);
                    for _ in 0..buf.len() {
                        wrt(b" ");
                    }
                    move_left(buf.len());
                    buf.clear();
                    cursor = 0;
                }

                0x09 => {
                    self.tab_complete(&mut buf, &mut cursor);
                }

                c if c >= 0x20 => {
                    buf.insert(cursor, c);
                    cursor += 1;
                    wrt(&buf[cursor - 1..]);
                    let tail = buf.len() - cursor;
                    if tail > 0 {
                        move_left(tail);
                    }
                }

                _ => {}
            }
        }
    }

    fn replace_input(old_buf: &[u8], old_cursor: usize, new_buf: &[u8]) {
        move_left(old_cursor);
        wrt(new_buf);
        if old_buf.len() > new_buf.len() {
            let extra = old_buf.len() - new_buf.len();
            for _ in 0..extra {
                wrt(b" ");
            }
            move_left(extra);
        }
    }

    // ── Tab completion ────────────────────────────────────────────────────────

    fn tab_complete(&self, buf: &mut Vec<u8>, cursor: &mut usize) {
        // Find start of the word ending at cursor
        let word_start = {
            let mut i = *cursor;
            while i > 0 && buf[i - 1] != b' ' && buf[i - 1] != b'\t' {
                i -= 1;
            }
            i
        };
        let prefix = buf[word_start..*cursor].to_vec();

        // Command context when everything before word_start is whitespace
        let is_cmd = buf[..word_start].iter().all(|&b| b == b' ' || b == b'\t');

        let matches = if is_cmd {
            self.complete_command(&prefix)
        } else {
            self.complete_path(&prefix)
        };

        match matches.len() {
            0 => {
                wrt(b"\x07"); // bell — no match
            }
            1 => {
                Self::apply_completion(buf, cursor, word_start, &matches[0]);
            }
            _ => {
                // Print all candidates on a new line, then redraw prompt + input
                wrt(b"\r\n");
                for m in &matches {
                    wrt(m);
                    wrt(b"  ");
                }
                wrt(b"\r\n");
                self.prompt();
                wrt(&buf[..*cursor]);
                let tail = buf.len() - *cursor;
                if tail > 0 {
                    wrt(&buf[*cursor..]);
                    move_left(tail);
                }
            }
        }
    }

    fn apply_completion(buf: &mut Vec<u8>, cursor: &mut usize, word_start: usize, completion: &[u8]) {
        let old_word_len = *cursor - word_start;
        let tail: Vec<u8> = buf[*cursor..].to_vec();

        if old_word_len > 0 {
            move_left(old_word_len);
        }
        wrt(completion);
        if !tail.is_empty() {
            wrt(&tail);
            move_left(tail.len());
        }
        // Erase any leftover chars if the old word was longer
        if old_word_len > completion.len() {
            let extra = old_word_len - completion.len();
            for _ in 0..extra {
                wrt(b" ");
            }
            move_left(extra);
        }

        buf.truncate(word_start);
        buf.extend_from_slice(completion);
        *cursor = buf.len();
        buf.extend_from_slice(&tail);
    }

    // Complete a command name: builtins + executables in /.
    fn complete_command(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        if prefix.starts_with(b"/") {
            return self.complete_path(prefix);
        }
        const BUILTINS: &[&[u8]] = &[
            b"echo", b"ls", b"cat", b"pwd", b"cd", b"mkdir", b"rm",
            b"help", b"clear", b"history", b"nano", b"whoami",
            b"hostname", b"uname", b"exit",
        ];
        let mut matches: Vec<Vec<u8>> = Vec::new();
        for &b in BUILTINS {
            if b.starts_with(prefix) {
                matches.push(b.to_vec());
            }
        }
        // Also scan / for executables
        let root = b"/";
        let mut dbuf = [0u8; 4096];
        let n = getdents(root.as_ptr(), root.len(), dbuf.as_mut_ptr(), dbuf.len());
        if n > 0 {
            let mut pos = 0usize;
            while pos < n as usize {
                let start = pos;
                while pos < n as usize && dbuf[pos] != 0 {
                    pos += 1;
                }
                if pos > start {
                    let entry = &dbuf[start..pos];
                    if entry.starts_with(prefix) && !matches.iter().any(|m| m.as_slice() == entry) {
                        matches.push(entry.to_vec());
                    }
                }
                pos += 1;
            }
        }
        matches
    }

    // Complete a path argument.
    fn complete_path(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        let (dir_path, name_prefix, path_prefix) =
            if let Some(pos) = prefix.iter().rposition(|&b| b == b'/') {
                let dir = &prefix[..=pos];
                let name = prefix[pos + 1..].to_vec();
                (self.resolve(dir), name, prefix[..=pos].to_vec())
            } else {
                (self.cwd.clone(), prefix.to_vec(), Vec::new())
            };

        let mut dbuf = [0u8; 4096];
        let n = getdents(dir_path.as_ptr(), dir_path.len(), dbuf.as_mut_ptr(), dbuf.len());
        if n < 0 {
            return Vec::new();
        }
        let mut matches = Vec::new();
        let mut pos = 0usize;
        while pos < n as usize {
            let start = pos;
            while pos < n as usize && dbuf[pos] != 0 {
                pos += 1;
            }
            if pos > start {
                let entry = &dbuf[start..pos];
                if entry != b"." && entry != b".." && entry.starts_with(name_prefix.as_slice()) {
                    let mut completion = path_prefix.clone();
                    completion.extend_from_slice(entry);
                    matches.push(completion);
                }
            }
            pos += 1;
        }
        matches
    }

    // Collapse `.` and `..` in an absolute path.
    fn normalize_path(path: &[u8]) -> Vec<u8> {
        let mut parts: Vec<&[u8]> = Vec::new();
        for component in path.split(|&b| b == b'/') {
            match component {
                b"" | b"." => {}
                b".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        let mut result = Vec::new();
        result.push(b'/');
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                result.push(b'/');
            }
            result.extend_from_slice(part);
        }
        result
    }

    fn is_builtin(cmd: &[u8]) -> bool {
        matches!(
            cmd,
            b"echo"
                | b"ls"
                | b"cat"
                | b"pwd"
                | b"cd"
                | b"mkdir"
                | b"rm"
                | b"help"
                | b"?"
                | b"clear"
                | b"history"
                | b".history"
                | b"nano"
                | b"whoami"
                | b"hostname"
                | b"uname"
                | b"uname-a"
                | b"export"
                | b"unset"
                | b"env"
                | b"printenv"
                | b"exit"
                | b"quit"
        )
    }

    // Tokenise `line` and split into pipeline stages.
    fn parse_pipeline(line: &[u8]) -> Vec<Stage> {
        // Phase 1: produce tokens (words + operators |  <  >  >>)
        let mut tokens: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut in_q = false;
        let mut qch = 0u8;
        let mut i = 0;
        while i < line.len() {
            let b = line[i];
            if in_q {
                if b == qch {
                    in_q = false;
                } else {
                    cur.push(b);
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' => {
                    in_q = true;
                    qch = b;
                    i += 1;
                }
                b'|' | b'<' => {
                    if !cur.is_empty() {
                        tokens.push(core::mem::take(&mut cur));
                    }
                    tokens.push(vec![b]);
                    i += 1;
                }
                b'>' => {
                    if !cur.is_empty() {
                        tokens.push(core::mem::take(&mut cur));
                    }
                    if i + 1 < line.len() && line[i + 1] == b'>' {
                        tokens.push(b">>".to_vec());
                        i += 2;
                    } else {
                        tokens.push(b">".to_vec());
                        i += 1;
                    }
                }
                b' ' | b'\t' => {
                    if !cur.is_empty() {
                        tokens.push(core::mem::take(&mut cur));
                    }
                    i += 1;
                }
                _ => {
                    cur.push(b);
                    i += 1;
                }
            }
        }
        if !cur.is_empty() {
            tokens.push(cur);
        }

        // Phase 2: build stages
        let mut stages: Vec<Stage> = Vec::new();
        let mut cur_stage = Stage {
            args: Vec::new(),
            stdin_file: None,
            stdout_file: None,
        };
        let mut j = 0;
        while j < tokens.len() {
            match tokens[j].as_slice() {
                b"|" => {
                    stages.push(cur_stage);
                    cur_stage = Stage {
                        args: Vec::new(),
                        stdin_file: None,
                        stdout_file: None,
                    };
                }
                b"<" => {
                    j += 1;
                    if j < tokens.len() {
                        cur_stage.stdin_file = Some(tokens[j].clone());
                    }
                }
                b">" | b">>" => {
                    j += 1;
                    if j < tokens.len() {
                        cur_stage.stdout_file = Some(tokens[j].clone());
                    }
                }
                _ => {
                    cur_stage.args.push(tokens[j].clone());
                }
            }
            j += 1;
        }
        if !cur_stage.args.is_empty() {
            stages.push(cur_stage);
        }
        stages
    }

    fn resolve(&self, p: &[u8]) -> Vec<u8> {
        let raw = if p.starts_with(b"/") {
            p.to_vec()
        } else {
            let mut full = self.cwd.clone();
            if !full.ends_with(b"/") {
                full.push(b'/');
            }
            full.extend_from_slice(p);
            full
        };
        Self::normalize_path(&raw)
    }

    // Redirect fd 1 to `outfile`; returns old fd (or -1 if no redirection).
    fn apply_out_redir(&self, outfile: &Option<Vec<u8>>) -> i32 {
        if let Some(f) = outfile {
            let saved = dup(1);
            let path = self.resolve(f);
            let fd = create(&path);
            if fd >= 0 {
                dup2(fd, 1);
                close(fd);
            }
            saved
        } else {
            -1
        }
    }

    // Redirect fd 0 from `infile`; returns old fd (or -1 if no redirection).
    fn apply_in_redir(&self, infile: &Option<Vec<u8>>) -> i32 {
        if let Some(f) = infile {
            let saved = dup(0);
            let path = self.resolve(f);
            let fd = open(path.as_ptr(), path.len(), 0);
            if fd >= 0 {
                dup2(fd, 0);
                close(fd);
            }
            saved
        } else {
            -1
        }
    }

    fn undo_redir(target_fd: i32, saved: i32) {
        if saved >= 0 {
            dup2(saved, target_fd);
            close(saved);
        }
    }

    fn build_path(cmd: &[u8]) -> Vec<u8> {
        let mut path = Vec::new();
        if !cmd.starts_with(b"/") {
            path.push(b'/');
        }
        path.extend_from_slice(cmd);
        path
    }

    fn exec_child(&self, args: &[Vec<u8>]) -> ! {
        if !args.is_empty() {
            let path = Self::build_path(&args[0]);
            // Pack args as null-terminated strings for the kernel argv setup.
            let mut argv_buf: Vec<u8> = Vec::new();
            for arg in args {
                argv_buf.extend_from_slice(arg);
                argv_buf.push(0);
            }
            exec_args(&path, &argv_buf); // returns only on failure
            write(2, b"sh: exec: ".as_ptr(), 10);
            write(2, args[0].as_ptr(), args[0].len());
            write(2, b"\n".as_ptr(), 1);
        }
        exit(127)
    }

    // Execute a single stage (no pipeline context).
    fn exec_single(&mut self, stage: &Stage) {
        if stage.args.is_empty() {
            return;
        }
        if Self::is_builtin(&stage.args[0]) {
            let saved_out = self.apply_out_redir(&stage.stdout_file);
            let saved_in = self.apply_in_redir(&stage.stdin_file);
            self.dispatch(&stage.args);
            Self::undo_redir(1, saved_out);
            Self::undo_redir(0, saved_in);
        } else {
            set_raw(0);
            let pid = fork();
            if pid == 0 {
                self.apply_out_redir(&stage.stdout_file);
                self.apply_in_redir(&stage.stdin_file);
                self.exec_child(&stage.args);
            } else if pid > 0 {
                let mut status = 0i32;
                set_fg_pid(pid);
                waitpid(pid, &mut status as *mut i32);
                set_fg_pid(-1);
            } else {
                wrt(b"sh: fork failed\n");
            }
            set_raw(1);
        }
    }

    // Execute a multi-stage pipeline.
    fn exec_pipeline(&mut self, stages: &[Stage]) {
        set_raw(0);
        let mut pids: Vec<i32> = Vec::new();
        let mut prev_read: i32 = -1;

        for (i, stage) in stages.iter().enumerate() {
            let is_last = i + 1 == stages.len();
            let (cur_read, cur_write) = if !is_last {
                let mut fds = [0u32; 2];
                pipe(&mut fds as *mut [u32; 2]);
                (fds[0] as i32, fds[1] as i32)
            } else {
                (-1, -1)
            };

            let pid = fork();
            if pid == 0 {
                // Wire up pipes
                if prev_read >= 0 {
                    dup2(prev_read, 0);
                    close(prev_read);
                }
                if cur_write >= 0 {
                    dup2(cur_write, 1);
                    close(cur_write);
                }
                if cur_read >= 0 {
                    close(cur_read);
                }
                // Explicit file redirections override pipe setup
                self.apply_out_redir(&stage.stdout_file);
                self.apply_in_redir(&stage.stdin_file);

                if Self::is_builtin(&stage.args[0]) {
                    self.dispatch(&stage.args);
                    exit(0);
                } else {
                    self.exec_child(&stage.args);
                }
            }

            // Parent: release pipe ends the child now owns
            if prev_read >= 0 {
                close(prev_read);
            }
            if cur_write >= 0 {
                close(cur_write);
            }
            prev_read = cur_read;
            if pid > 0 {
                pids.push(pid);
            }
        }

        for pid in &pids {
            set_fg_pid(*pid);
            let mut status = 0i32;
            waitpid(*pid, &mut status as *mut i32);
        }
        set_fg_pid(-1);
        set_raw(1);
    }

    // Dispatch builtins; external commands print an error (use exec_single/exec_pipeline instead).
    fn dispatch(&mut self, args: &[Vec<u8>]) {
        if args.is_empty() {
            return;
        }
        let cmd = args[0].as_slice();
        // --help flag: show usage and return
        if args.iter().any(|a| a.as_slice() == b"--help") {
            Self::show_cmd_help(cmd);
            return;
        }
        match cmd {
            b"echo" => self.cmd_echo(&args[1..]),
            b"ls" => self.cmd_ls(&args[1..]),
            b"cat" => self.cmd_cat(&args[1..]),
            b"pwd" => {
                wrt(&self.cwd);
                wrt_nl();
            }
            b"cd" => self.cmd_cd(&args[1..]),
            b"mkdir" => self.cmd_mkdir(&args[1..]),
            b"rm" => self.cmd_rm(&args[1..]),
            b"help" | b"?" => self.cmd_help(),
            b"clear" => wrt(CLR),
            b"history" | b".history" => self.cmd_history(),
            b"nano" => {
                set_raw(0);
                self.cmd_nano(&args[1..]);
                set_raw(1);
            }
            b"whoami" => wrt(b"guest\n"),
            b"hostname" => wrt(b"openv\n"),
            b"uname" | b"uname-a" => wrt(b"openv 0.1.0 riscv64gc-unknown-none-elf\n"),
            b"export" => self.cmd_export(&args[1..]),
            b"unset" => self.cmd_unset(&args[1..]),
            b"env" | b"printenv" => self.cmd_env(),
            b"exit" | b"quit" => {
                set_raw(0);
                exit(0);
            }
            _ => {
                wrt(b"sh: ");
                wrt(cmd);
                wrt(b": command not found\n");
            }
        }
    }

    // ── Builtins ──────────────────────────────────────────────────────────────

    fn show_cmd_help(cmd: &[u8]) {
        match cmd {
            b"echo" => {
                wrt(b"Usage: echo [arg...]\n");
                wrt(b"  Print arguments to stdout separated by spaces.\n");
            }
            b"ls" => {
                wrt(b"Usage: ls [path]\n");
                wrt(b"  List directory contents.\n");
                wrt(b"  With no argument, lists the current directory.\n");
            }
            b"cat" => {
                wrt(b"Usage: cat [file...]\n");
                wrt(b"  Concatenate files and print to stdout.\n");
                wrt(b"  With no arguments, reads from stdin until EOF (Ctrl-D).\n");
            }
            b"pwd" => {
                wrt(b"Usage: pwd\n");
                wrt(b"  Print the current working directory.\n");
            }
            b"cd" => {
                wrt(b"Usage: cd [path]\n");
                wrt(b"  Change the current directory.\n");
                wrt(b"  With no argument, changes to /.\n");
            }
            b"mkdir" => {
                wrt(b"Usage: mkdir <dir...>\n");
                wrt(b"  Create one or more directories.\n");
            }
            b"rm" => {
                wrt(b"Usage: rm <file...>\n");
                wrt(b"  Remove one or more files.\n");
            }
            b"nano" => {
                wrt(b"Usage: nano <file>\n");
                wrt(b"  Open a file in the text editor.\n");
                wrt(b"  ^O: save   ^X: exit   ^K: delete line\n");
            }
            b"history" | b".history" => {
                wrt(b"Usage: history\n");
                wrt(b"  Display the command history list.\n");
            }
            b"clear" => {
                wrt(b"Usage: clear\n");
                wrt(b"  Clear the terminal screen.\n");
            }
            b"whoami" => {
                wrt(b"Usage: whoami\n");
                wrt(b"  Print the current user name.\n");
            }
            b"hostname" => {
                wrt(b"Usage: hostname\n");
                wrt(b"  Print the system hostname.\n");
            }
            b"uname" | b"uname-a" => {
                wrt(b"Usage: uname\n");
                wrt(b"  Print system information (OS, version, architecture).\n");
            }
            b"exit" | b"quit" => {
                wrt(b"Usage: exit\n");
                wrt(b"  Exit the shell.\n");
            }
            b"help" | b"?" => {
                wrt(b"Usage: help\n");
                wrt(b"  Show a summary of all built-in commands.\n");
            }
            _ => {
                wrt(b"No help available for '");
                wrt(cmd);
                wrt(b"'.\n");
            }
        }
    }

    fn cmd_echo(&self, args: &[Vec<u8>]) {
        let mut first = true;
        for arg in args {
            if !first {
                wrt(b" ");
            }
            first = false;
            wrt(arg);
        }
        wrt_nl();
    }

    fn cmd_ls(&self, args: &[Vec<u8>]) {
        let path = if args.is_empty() {
            self.cwd.clone()
        } else {
            self.resolve(&args[0])
        };
        let mut buf = [0u8; 4096];
        let n = getdents(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len());
        if n < 0 {
            wrt(b"ls: cannot access '");
            wrt(&path);
            wrt(b"': No such file or directory\n");
            return;
        }
        let mut pos = 0usize;
        while pos < n as usize {
            let start = pos;
            while pos < n as usize && buf[pos] != 0 {
                pos += 1;
            }
            if pos > start {
                wrt(CYAN);
                wrt(&buf[start..pos]);
                wrt(RESET);
                wrt(b"\n");
            }
            pos += 1;
        }
    }

    fn cmd_cat(&self, args: &[Vec<u8>]) {
        if args.is_empty() {
            // Read from stdin until EOF
            let mut buf = [0u8; 1024];
            loop {
                let n = read(0, buf.as_mut_ptr(), buf.len());
                if n <= 0 {
                    break;
                }
                wrt(&buf[..n as usize]);
            }
            return;
        }
        for arg in args {
            let path = self.resolve(arg);
            let fd = open(path.as_ptr(), path.len(), 0);
            if fd < 0 {
                wrt(b"cat: ");
                wrt(&path);
                wrt(b": No such file or directory\n");
                continue;
            }
            let mut buf = [0u8; 1024];
            loop {
                let n = read(fd as usize, buf.as_mut_ptr(), buf.len());
                if n <= 0 {
                    break;
                }
                wrt(&buf[..n as usize]);
            }
            close(fd);
        }
    }

    fn cmd_cd(&mut self, args: &[Vec<u8>]) {
        let path = if args.is_empty() {
            b"/".to_vec()
        } else {
            self.resolve(&args[0])
        };
        if chdir(&path) == 0 {
            self.env_set(b"PWD".to_vec(), path.clone());
            self.cwd = path;
        } else {
            wrt(b"cd: ");
            wrt(&path);
            wrt(b": No such file or directory\n");
        }
    }

    fn cmd_mkdir(&self, args: &[Vec<u8>]) {
        if args.is_empty() {
            wrt(b"mkdir: missing operand\n");
            return;
        }
        for arg in args {
            let path = self.resolve(arg);
            if mkdir(&path) != 0 {
                wrt(b"mkdir: cannot create directory '");
                wrt(&path);
                wrt(b"'\n");
            }
        }
    }

    fn cmd_rm(&self, args: &[Vec<u8>]) {
        if args.is_empty() {
            wrt(b"rm: missing operand\n");
            return;
        }
        for arg in args {
            let path = self.resolve(arg);
            if unlink(&path) != 0 {
                wrt(b"rm: cannot remove '");
                wrt(&path);
                wrt(b"': No such file or directory\n");
            }
        }
    }

    fn cmd_export(&mut self, args: &[Vec<u8>]) {
        if args.is_empty() {
            self.cmd_env();
            return;
        }
        for arg in args {
            if let Some((name, value)) = Self::parse_assignment(arg) {
                let expanded = Self::expand_vars(&value, &self.env);
                self.env_set(name, expanded);
            } else {
                // `export NAME` with no value: ensure it exists (set empty if absent)
                let name = arg.clone();
                if self.env_get(&name).is_none() {
                    self.env_set(name, Vec::new());
                }
            }
        }
    }

    fn cmd_unset(&mut self, args: &[Vec<u8>]) {
        for arg in args {
            self.env_unset(arg);
        }
    }

    fn cmd_env(&self) {
        for (name, value) in &self.env {
            wrt(name);
            wrt(b"=");
            wrt(value);
            wrt_nl();
        }
    }

    fn cmd_history(&self) {
        if self.history.is_empty() {
            wrt(b"(no history)\n");
            return;
        }
        for (i, entry) in self.history.iter().enumerate() {
            let idx = i + 1;
            if idx < 10 {
                wrt(b"   ");
            } else if idx < 100 {
                wrt(b"  ");
            } else if idx < 1000 {
                wrt(b" ");
            }
            wrt_num(idx);
            wrt(b"  ");
            wrt(entry);
            wrt_nl();
        }
    }

    fn cmd_help(&self) {
        wrt(BOLD);
        wrt(b"openv shell builtins:\n");
        wrt(RESET);
        let cmds = [
            ("echo [args]",        "Print arguments to stdout"),
            ("ls [path]",          "List directory contents"),
            ("cat [file]",         "Print file (or stdin) to stdout"),
            ("pwd",                "Print working directory"),
            ("cd [path]",          "Change directory (default /)"),
            ("mkdir <dir>",        "Create directory"),
            ("rm <file>",          "Remove file or directory"),
            ("nano <file>",        "Text editor (^O save, ^X exit)"),
            ("export [NAME=VAL]",  "Set/show environment variables"),
            ("unset <NAME>",       "Remove an environment variable"),
            ("env",                "List all environment variables"),
            ("history",            "Show command history"),
            ("clear",              "Clear the screen"),
            ("whoami",             "Print current user"),
            ("hostname",           "Print hostname"),
            ("uname",              "Print system info"),
            ("exit",               "Exit the shell"),
        ];
        wrt(b"  Pipelines: cmd | cmd    Redirect: > file  < file\n");
        wrt(b"  Editing: Up/Down=history  Left/Right=move  Ctrl-A/E=start/end  Ctrl-U=kill\n\n");
        for (cmd, desc) in &cmds {
            wrt(b"  ");
            wrt(CYAN);
            wrt_str(cmd);
            wrt(RESET);
            let pad = 18usize.saturating_sub(cmd.len());
            for _ in 0..pad {
                wrt(b" ");
            }
            wrt_str(desc);
            wrt_nl();
        }
    }

    fn cmd_nano(&self, args: &[Vec<u8>]) {
        if args.is_empty() {
            wrt(b"nano: missing filename\n");
            return;
        }
        let path = self.resolve(&args[0]);
        NanoEditor::new(path).run();
    }

    fn run(&mut self) {
        set_raw(1);
        loop {
            self.prompt();
            let line = self.read_line_raw();
            if line.is_empty() {
                continue;
            }
            if self.history.last().map(|e| e.as_slice()) != Some(line.as_slice()) {
                self.history.push(line.clone());
            }
            // Bare `NAME=value` — assignment only, no command.
            let trimmed = {
                let mut s = line.as_slice();
                while s.first() == Some(&b' ') || s.first() == Some(&b'\t') { s = &s[1..]; }
                s
            };
            if let Some((name, value)) = Self::parse_assignment(trimmed) {
                let expanded = Self::expand_vars(&value, &self.env);
                self.env_set(name, expanded);
                continue;
            }
            // Expand $VAR references, then parse and execute.
            let expanded = Self::expand_vars(&line, &self.env);
            let stages = Self::parse_pipeline(&expanded);
            match stages.len() {
                0 => continue,
                1 => self.exec_single(&stages[0]),
                _ => self.exec_pipeline(&stages),
            }
        }
    }
}

// ── Nano editor ───────────────────────────────────────────────────────────────

const TERM_ROWS: usize = 24;
const TERM_COLS: usize = 80;
const CONTENT_ROWS: usize = TERM_ROWS - 3;

fn read_raw_byte() -> u8 {
    let mut c = 0u8;
    loop {
        let n = read(0, &mut c as *mut u8, 1);
        if n > 0 {
            return c;
        }
        sys_yield();
    }
}

fn read_key() -> NanoKey {
    let c = read_raw_byte();
    match c {
        0x1b => {
            let b1 = read_raw_byte();
            if b1 != b'[' {
                return NanoKey::Other;
            }
            let b2 = read_raw_byte();
            match b2 {
                b'A' => NanoKey::Up,
                b'B' => NanoKey::Down,
                b'C' => NanoKey::Right,
                b'D' => NanoKey::Left,
                b'H' => NanoKey::Home,
                b'F' => NanoKey::End,
                b'3' => {
                    read_raw_byte();
                    NanoKey::Delete
                }
                b'5' => {
                    read_raw_byte();
                    NanoKey::PageUp
                }
                b'6' => {
                    read_raw_byte();
                    NanoKey::PageDown
                }
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
    Enter,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    CtrlO,
    CtrlX,
    CtrlK,
    Other,
}

fn move_cursor(row: usize, col: usize) {
    let mut buf = [0u8; 16];
    buf[0] = 0x1b;
    buf[1] = b'[';
    let mut p = 2;
    p += fmt_decimal(&mut buf[p..], row);
    buf[p] = b';';
    p += 1;
    p += fmt_decimal(&mut buf[p..], col);
    buf[p] = b'H';
    p += 1;
    wrt(&buf[..p]);
}

fn pad_line(used: usize) {
    let n = TERM_COLS.saturating_sub(used);
    for _ in 0..n {
        wrt(b" ");
    }
    wrt(RESET);
}

struct NanoEditor {
    filename: Vec<u8>,
    lines: Vec<Vec<u8>>,
    row: usize,
    col: usize,
    scroll: usize,
    modified: bool,
    msg: Vec<u8>,
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
                if n <= 0 {
                    break;
                }
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
            if start < content.len() {
                lines.push(content[start..].to_vec());
            }
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        NanoEditor {
            filename,
            lines,
            row: 0,
            col: 0,
            scroll: 0,
            modified: false,
            msg: Vec::new(),
        }
    }

    fn render(&self) {
        wrt(b"\x1b[?25l");
        wrt(CLR);
        move_cursor(1, 1);
        wrt(REV);
        wrt(BOLD);
        wrt(b"  openv nano 0.1   ");
        wrt(&self.filename);
        if self.modified {
            wrt(b"  [Modified]");
        }
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
            wrt(YELLOW);
            wrt(&self.msg);
            wrt(RESET);
        } else {
            wrt(b"Ln ");
            wrt_num(self.row + 1);
            wrt(b", Col ");
            wrt_num(self.col + 1);
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
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + CONTENT_ROWS {
            self.scroll = self.row + 1 - CONTENT_ROWS;
        }
    }

    fn clamp_col(&mut self) {
        let len = self.lines[self.row].len();
        if self.col > len {
            self.col = len;
        }
    }

    fn save(&mut self) {
        let fd = create(&self.filename);
        if fd < 0 {
            self.msg = b"Error: cannot write file!".to_vec();
            return;
        }
        for (i, line) in self.lines.iter().enumerate() {
            write(fd as usize, line.as_ptr(), line.len());
            if i + 1 < self.lines.len() {
                write(fd as usize, b"\n".as_ptr(), 1);
            }
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
                if self.lines.is_empty() {
                    self.lines.push(Vec::new());
                }
                if self.row >= self.lines.len() {
                    self.row = self.lines.len() - 1;
                }
                self.clamp_col();
                self.modified = true;
            }
            NanoKey::Up => {
                if self.row > 0 {
                    self.row -= 1;
                    self.clamp_col();
                    self.adjust_scroll();
                }
            }
            NanoKey::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.clamp_col();
                    self.adjust_scroll();
                }
            }
            NanoKey::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                    self.adjust_scroll();
                }
            }
            NanoKey::Right => {
                let len = self.lines[self.row].len();
                if self.col < len {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                    self.adjust_scroll();
                }
            }
            NanoKey::Home => {
                self.col = 0;
            }
            NanoKey::End => {
                self.col = self.lines[self.row].len();
            }
            NanoKey::PageUp => {
                self.row = self.row.saturating_sub(CONTENT_ROWS);
                self.clamp_col();
                self.adjust_scroll();
            }
            NanoKey::PageDown => {
                self.row = (self.row + CONTENT_ROWS).min(self.lines.len() - 1);
                self.clamp_col();
                self.adjust_scroll();
            }
            NanoKey::Enter => {
                let rest = self.lines[self.row].split_off(self.col);
                self.row += 1;
                self.lines.insert(self.row, rest);
                self.col = 0;
                self.adjust_scroll();
                self.modified = true;
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
                    self.adjust_scroll();
                    self.modified = true;
                }
            }
            NanoKey::Delete => {
                let len = self.lines[self.row].len();
                if self.col < len {
                    self.lines[self.row].remove(self.col);
                    self.modified = true;
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].extend_from_slice(&next);
                    self.modified = true;
                }
            }
            NanoKey::Char(c) => {
                self.lines[self.row].insert(self.col, c);
                self.col += 1;
                self.modified = true;
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
            if !self.handle(key) {
                break;
            }
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
