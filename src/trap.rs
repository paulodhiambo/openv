use crate::println;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::Ordering;
use riscv::register::{scause, sepc, stvec};

unsafe extern "C" {
    fn __halt_cpu() -> !;
}
use crate::sync::Mutex;
use core::sync::atomic::{AtomicBool, AtomicI32};

/// PID of the registered VFS server process (0 = not yet started).
pub static VFS_SERVER_PID: AtomicI32 = AtomicI32::new(0);

static LINE_DISC_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ECHO_ENABLED: Mutex<bool> = Mutex::new(true);
static RAW_MODE: Mutex<bool> = Mutex::new(false);
// PID of the process blocked waiting for stdin (0 = nobody).
static STDIN_WAITER: AtomicI32 = AtomicI32::new(0);
// Set when Ctrl+C arrives and no external foreground process exists.
// Causes the next Console sys_read to return 0 (EOF / EINTR) so builtins like
// `cat` that loop on n <= 0 exit cleanly.
static CTRLC_PENDING: AtomicBool = AtomicBool::new(false);

macro_rules! get_current_proc_or_esrch {
    ($tf:expr) => {
        match crate::posix::process::get_current_proc() {
            Some(p) => p,
            None => {
                $tf.regs[10] = crate::errno::ESRCH;
                $tf.sepc += 4;
                return $tf as *mut _;
            }
        }
    };
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub kernel_sp: usize,
    pub regs: [usize; 32], // x0 to x31
    pub sepc: usize,
    pub sstatus: usize,
}

impl TrapFrame {
    pub const fn new() -> Self {
        TrapFrame {
            kernel_sp: 0,
            regs: [0; 32],
            sepc: 0,
            sstatus: 0,
        }
    }
}

global_asm!(
    r#"
    .section .text
    .global trap_vector
    .align 4
trap_vector:
    # Swap a0 and sscratch
    csrrw a0, sscratch, a0
    bnez a0, 1f

    # --- Came from S-mode ---
    csrrw a0, sscratch, a0 # Restore a0, sscratch is 0
    addi sp, sp, -288
    sd a0, 10*8+8(sp)
    sd t0, 5*8+8(sp)
    addi t0, sp, 288
    sd t0, 2*8+8(sp)
    j 2f

1:
    # --- Came from U-mode ---
    sd t0, 5*8+8(a0)
    csrr t0, sscratch
    sd t0, 10*8+8(a0)
    csrw sscratch, zero # Indicate S-mode
    sd sp, 2*8+8(a0)
    mv sp, a0

2:
    sd ra, 1*8+8(sp)
    sd gp, 3*8+8(sp)
    sd tp, 4*8+8(sp)
    sd t1, 6*8+8(sp)
    sd t2, 7*8+8(sp)
    sd s0, 8*8+8(sp)
    sd s1, 9*8+8(sp)
    sd a1, 11*8+8(sp)
    sd a2, 12*8+8(sp)
    sd a3, 13*8+8(sp)
    sd a4, 14*8+8(sp)
    sd a5, 15*8+8(sp)
    sd a6, 16*8+8(sp)
    sd a7, 17*8+8(sp)
    sd s2, 18*8+8(sp)
    sd s3, 19*8+8(sp)
    sd s4, 20*8+8(sp)
    sd s5, 21*8+8(sp)
    sd s6, 22*8+8(sp)
    sd s7, 23*8+8(sp)
    sd s8, 24*8+8(sp)
    sd s9, 25*8+8(sp)
    sd s10, 26*8+8(sp)
    sd s11, 27*8+8(sp)
    sd t3, 28*8+8(sp)
    sd t4, 29*8+8(sp)
    sd t5, 30*8+8(sp)
    sd t6, 31*8+8(sp)

    csrr t0, sepc
    sd t0, 32*8+8(sp)
    csrr t1, sstatus
    sd t1, 33*8+8(sp)

    mv a0, sp

    csrr t0, sstatus
    andi t0, t1, 0x100
    bnez t0, 3f
    
    ld sp, 0(a0)
3:
    call rust_trap_handler
    
    .global return_to_user
return_to_user:
    mv sp, a0

    ld t0, 32*8+8(sp)
    csrw sepc, t0
    ld t1, 33*8+8(sp)
    csrw sstatus, t1

    ld ra, 1*8+8(sp)
    ld gp, 3*8+8(sp)
    ld tp, 4*8+8(sp)
    ld t1, 6*8+8(sp)
    ld t2, 7*8+8(sp)
    ld s0, 8*8+8(sp)
    ld s1, 9*8+8(sp)
    ld a0, 10*8+8(sp)
    ld a1, 11*8+8(sp)
    ld a2, 12*8+8(sp)
    ld a3, 13*8+8(sp)
    ld a4, 14*8+8(sp)
    ld a5, 15*8+8(sp)
    ld a6, 16*8+8(sp)
    ld a7, 17*8+8(sp)
    ld s2, 18*8+8(sp)
    ld s3, 19*8+8(sp)
    ld s4, 20*8+8(sp)
    ld s5, 21*8+8(sp)
    ld s6, 22*8+8(sp)
    ld s7, 23*8+8(sp)
    ld s8, 24*8+8(sp)
    ld s9, 25*8+8(sp)
    ld s10, 26*8+8(sp)
    ld s11, 27*8+8(sp)
    ld t3, 28*8+8(sp)
    ld t4, 29*8+8(sp)
    ld t5, 30*8+8(sp)
    ld t6, 31*8+8(sp)

    csrr t0, sstatus
    andi t0, t0, 0x100
    bnez t0, 4f

    ld t0, 2*8+8(sp)
    csrw sscratch, t0
    ld t0, 5*8+8(sp)
    csrrw sp, sscratch, sp
    sret

4:
    ld t0, 5*8+8(sp)
    ld sp, 2*8+8(sp)
    sret
    "#
);

unsafe extern "C" {
    fn trap_vector();
    pub fn return_to_user(trap_frame: usize) -> !;
}

/// Set up argc/argv on the new user stack after exec, returning (argc, argv_ptr, new_sp).
///
/// `argv_data` is a packed buffer of null-terminated strings: "arg0\0arg1\0...argN\0".
/// Strings and the argv[] pointer array are written just below USER_STACK_TOP.
/// S-mode can access U-mode pages because sstatus.SUM=1 (set when returning to user mode
/// and left set through the trap, so the kernel trap handler inherits it).
fn setup_exec_stack(argv_data: &[u8]) -> (usize, usize, usize) {
    use crate::posix::spawn::USER_STACK_TOP;

    // Parse null-separated args from the packed buffer.
    let mut args: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    for (i, &b) in argv_data.iter().enumerate() {
        if b == 0 {
            if i > start {
                args.push(&argv_data[start..i]);
            }
            start = i + 1;
        }
    }
    if start < argv_data.len() {
        args.push(&argv_data[start..]);
    }

    let argc = args.len();
    if argc == 0 {
        return (0, 0, USER_STACK_TOP);
    }

    let mut sp = USER_STACK_TOP;
    let mut string_addrs: Vec<usize> = Vec::new();

    // Write each arg string from the top of the stack downward (including null terminator).
    for arg in &args {
        let len = arg.len() + 1;
        sp -= len;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(sp as *mut u8, len);
            dst[..arg.len()].copy_from_slice(arg);
            dst[arg.len()] = 0;
        }
        string_addrs.push(sp);
    }

    // 8-byte align before writing the pointer array.
    sp &= !7;

    // Write argv[argc] = NULL first, then argv[N-1] .. argv[0] downward.
    sp -= 8;
    unsafe {
        *(sp as *mut usize) = 0;
    }
    for &addr in string_addrs.iter().rev() {
        sp -= 8;
        unsafe {
            *(sp as *mut usize) = addr;
        }
    }

    let argv_ptr = sp;

    // Leave 16 bytes below argv[0] and 16-byte align.
    sp = (sp - 16) & !15;

    (argc, argv_ptr, sp)
}

/// Drain all pending UART bytes into LINE_DISC_BUFFER and wake any blocked
/// stdin reader.  Called both from the timer ISR (HART 0 only) and from
/// schedule()'s idle loop so chars are serviced even when SIE=0 in S-mode.
fn poll_uart_into_linedisc() {
    let mut uart = crate::uart::Uart::new();
    let mut any_pushed = false;
    loop {
        let c = match uart.try_get_char() {
            Some(c) => c,
            None => break,
        };
        if c == 0x03 {
            let fg = crate::posix::process::FOREGROUND_PID.swap(-1, Ordering::Relaxed);
            uart.put_char(b'^');
            uart.put_char(b'C');
            uart.put_char(b'\n');
            if fg > 0 {
                // Kill the external foreground process (e.g. a spawned child).
                crate::posix::spawn::exit(fg, 130);
            } else {
                // No external fg process — unblock any Console reader (e.g. builtin cat).
                CTRLC_PENDING.store(true, Ordering::Relaxed);
                let waiter = STDIN_WAITER.swap(0, Ordering::Relaxed);
                if waiter > 0 {
                    crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
                }
            }
            break;
        }
        let mut buf = LINE_DISC_BUFFER.lock();
        let echo = *ECHO_ENABLED.lock();
        let raw = *RAW_MODE.lock();
        let pushed = if raw {
            buf.push(c);
            true
        } else if c == b'\r' || c == b'\n' {
            if echo {
                uart.put_char(b'\n');
            }
            buf.push(b'\n');
            true
        } else if c == 0x08 || c == 0x7F {
            if buf.pop().is_some() && echo {
                uart.put_char(0x08);
                uart.put_char(b' ');
                uart.put_char(0x08);
            }
            false
        } else {
            if echo {
                uart.put_char(c);
            }
            buf.push(c);
            true
        };
        drop(buf);
        if pushed {
            any_pushed = true;
        }
    }
    if any_pushed {
        let waiter = STDIN_WAITER.swap(0, Ordering::Relaxed);
        if waiter > 0 {
            crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
        }
    }
}

/// Called from schedule()'s idle loop so UART chars are serviced when the
/// timer ISR cannot fire (SIE=0 in supervisor mode).
pub fn service_uart() {
    if crate::smp::current_hartid() == 0 {
        poll_uart_into_linedisc();
    }
}

pub fn init() {
    unsafe {
        stvec::write(trap_vector as *const () as usize, stvec::TrapMode::Direct);
        // Enable supervisor software interrupts (for TLB shootdown).
        // Timer interrupts are NOT enabled here — they are enabled right
        // before the first call to schedule() to avoid firing while
        // sscratch is still 0 (which would corrupt address zero in the
        // trap vector's csrrw sp, sscratch, sp).
        riscv::register::sie::set_ssoft();
    }
    println!("Trap handler initialized.");
}

/// Called by secondary HARTs from secondary_kmain — same as init() without the print.
pub fn init_hart() {
    unsafe {
        stvec::write(trap_vector as *const () as usize, stvec::TrapMode::Direct);
        riscv::register::sie::set_ssoft();
    }
}

/// Enable timer interrupts and arm the first timer.
/// Must be called right before the first schedule(), after sscratch has
/// been initialised by a prior return_to_user or kernel-mode scratch page.
pub fn enable_timer() {
    unsafe {
        riscv::register::sie::set_stimer();
    }
    crate::timer::set_next_timer();
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_trap_handler(tf: &mut TrapFrame) -> *mut TrapFrame {
    let cause = scause::read().cause();
    let sepc = sepc::read();
    let stval = riscv::register::stval::read();

    match cause {
        scause::Trap::Exception(8) => {
            // User ecall
            let syscall_num = tf.regs[17]; // a7
            let arg0 = tf.regs[10]; // a0
            let arg1 = tf.regs[11]; // a1
            let arg2 = tf.regs[12]; // a2

            // Advance sepc to next instruction after ecall unconditionally.
            // Blocking syscalls that diverge to schedule() and wish to restart
            // must subtract 4 from tf.sepc before yielding.
            tf.sepc += 4;

            match syscall_num {
                0 => {
                    // sys_yield — only switch if another process is actually waiting.
                    // When alone, fall through so sepc advances and we return to the
                    // caller immediately (avoids a pointless sfence.vma + context switch).
                    let mut rq = crate::posix::process::RUN_QUEUE.lock();
                    if !rq.is_empty() {
                        rq.push_back(crate::posix::process::current_pid());
                        drop(rq);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                    // else: fall through — bottom of Exception(8) arm advances sepc
                }
                1 => {
                    // sys_exit
                    crate::println!(
                        "Process {} exited with code {}",
                        crate::posix::process::current_pid(),
                        arg0
                    );
                    crate::posix::spawn::exit(crate::posix::process::current_pid(), arg0 as i32);
                    crate::posix::process::schedule();
                    unsafe { __halt_cpu() }
                }
                2 => {
                    // sys_write
                    // arg0 = fd, arg1 = buf ptr, arg2 = len
                    let buf = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2) };
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(arg0 as u32) {
                        match obj {
                            crate::ipc::handle::KernelObject::Console => {
                                if let Ok(s) = core::str::from_utf8(buf) {
                                    crate::print!("{}", s);
                                }
                                tf.regs[10] = arg2; // Return bytes written
                            }
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let msg = crate::ipc::channel::Message {
                                    bytes: alloc::vec::Vec::from(buf),
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = arg2,
                                    Err(_) => tf.regs[10] = usize::MAX, // -1
                                }
                            }
                            crate::ipc::handle::KernelObject::File(fd_desc) => {
                                let buf =
                                    unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2) };
                                let mut offset = fd_desc.offset.lock();
                                match fd_desc.vnode.write_at(*offset, buf) {
                                    Ok(n) => {
                                        *offset += n;
                                        tf.regs[10] = n;
                                    }
                                    Err(_) => {
                                        tf.regs[10] = usize::MAX;
                                    }
                                }
                            }
                            crate::ipc::handle::KernelObject::PipeWrite(half) => {
                                let mut data = half.data.lock();
                                for &b in buf {
                                    data.push_back(b);
                                }
                                drop(data);
                                tf.regs[10] = arg2;
                                // Wake any process blocked on the read end.
                                let waiter = half.waiter.swap(0, Ordering::Relaxed);
                                if waiter > 0 {
                                    crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
                                }
                            }
                            _ => {
                                tf.regs[10] = usize::MAX; // EBADF
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // Error
                    }
                }
                3 => {
                    // sys_pipe — arg0 = ptr to [u32; 2] filled with [read_fd, write_fd]
                    let (rh, wh) = crate::ipc::handle::create_pipe();
                    let proc = get_current_proc_or_esrch!(tf);
                    let mut fds = proc.fds.lock();
                    let h_r = {
                        let h = fds.lowest_free();
                        fds.insert_at(h, crate::ipc::handle::KernelObject::PipeRead(rh));
                        h
                    };
                    let h_w = {
                        let h = fds.lowest_free();
                        fds.insert_at(h, crate::ipc::handle::KernelObject::PipeWrite(wh));
                        h
                    };
                    unsafe {
                        let arr = arg0 as *mut [u32; 2];
                        (*arr)[0] = h_r;
                        (*arr)[1] = h_w;
                    }
                    tf.regs[10] = 0;
                }
                5 => {
                    // sys_read
                    // arg0 = fd, arg1 = buf ptr, arg2 = max_len
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(arg0 as u32) {
                        match obj {
                            crate::ipc::handle::KernelObject::Console => {
                                // Ctrl+C was pressed while we were blocked — return EOF so
                                // builtin loops (e.g. `cat` with no args) exit cleanly.
                                if CTRLC_PENDING.swap(false, Ordering::Relaxed) {
                                    tf.regs[10] = 0;
                                    // sepc was already advanced by the +4 at the top of
                                    // Exception(8); just fall through.
                                } else {
                                    // The timer ISR is the sole UART reader.  It fills
                                    // LINE_DISC_BUFFER (handling echo, backspace, newlines)
                                    // and wakes STDIN_WAITER.  sys_read only reads the buffer.
                                    let raw = *RAW_MODE.lock();
                                    let mut buf_lock = LINE_DISC_BUFFER.lock();

                                    let ready = if raw {
                                        // Raw mode: any byte satisfies the read.
                                        !buf_lock.is_empty()
                                    } else {
                                        // Cooked mode: wait for a complete line ('\n').
                                        buf_lock.iter().any(|&b| b == b'\n')
                                    };

                                    if ready {
                                        let len = if raw {
                                            // Return up to arg2 bytes.
                                            let take = core::cmp::min(arg2, buf_lock.len());
                                            let user_buf = unsafe {
                                                core::slice::from_raw_parts_mut(
                                                    arg1 as *mut u8,
                                                    take,
                                                )
                                            };
                                            for i in 0..take {
                                                user_buf[i] = buf_lock[i];
                                            }
                                            buf_lock.drain(0..take);
                                            take
                                        } else {
                                            // Return up to and including the first '\n'.
                                            let nl =
                                                buf_lock.iter().position(|&b| b == b'\n').unwrap();
                                            let take = core::cmp::min(arg2, nl + 1);
                                            let user_buf = unsafe {
                                                core::slice::from_raw_parts_mut(
                                                    arg1 as *mut u8,
                                                    take,
                                                )
                                            };
                                            for i in 0..take {
                                                user_buf[i] = buf_lock[i];
                                            }
                                            buf_lock.drain(0..take);
                                            take
                                        };
                                        tf.regs[10] = len;
                                    } else {
                                        // Buffer not ready — block.  The timer ISR will fill
                                        // LINE_DISC_BUFFER and wake us via STDIN_WAITER.
                                        drop(buf_lock);
                                        drop(fds);
                                        drop(proc);
                                        tf.sepc -= 4;
                                        STDIN_WAITER.store(
                                            crate::posix::process::current_pid(),
                                            Ordering::Relaxed,
                                        );
                                        crate::posix::process::schedule();
                                        unsafe { __halt_cpu() }
                                    }
                                }
                            }
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                if let Some(msg) = ep.try_recv() {
                                    let len = core::cmp::min(arg2, msg.bytes.len());
                                    let buf = unsafe {
                                        core::slice::from_raw_parts_mut(arg1 as *mut u8, len)
                                    };
                                    buf.copy_from_slice(&msg.bytes[..len]);
                                    tf.regs[10] = len;
                                } else {
                                    // Block by restarting the syscall
                                    drop(fds);
                                    drop(proc);
                                    tf.sepc -= 4; // Rewind to ecall
                                    crate::posix::process::RUN_QUEUE
                                        .lock()
                                        .push_back(crate::posix::process::current_pid());
                                    crate::posix::process::schedule();
                                    unsafe { __halt_cpu() }
                                }
                            }
                            crate::ipc::handle::KernelObject::File(fd_desc) => {
                                let mut offset = fd_desc.offset.lock();
                                let buf = unsafe {
                                    core::slice::from_raw_parts_mut(arg1 as *mut u8, arg2)
                                };
                                match fd_desc.vnode.read(*offset, buf) {
                                    Ok(bytes_read) => {
                                        *offset += bytes_read as usize;
                                        tf.regs[10] = bytes_read;
                                    }
                                    Err(_) => {
                                        tf.regs[10] = usize::MAX;
                                    }
                                }
                            }
                            crate::ipc::handle::KernelObject::PipeRead(half) => {
                                let mut data = half.data.lock();
                                if !data.is_empty() {
                                    let len = core::cmp::min(arg2, data.len());
                                    let buf = unsafe {
                                        core::slice::from_raw_parts_mut(arg1 as *mut u8, len)
                                    };
                                    for b in buf.iter_mut() {
                                        *b = data.pop_front().unwrap_or(0);
                                    }
                                    tf.regs[10] = len;
                                } else if half.write_open.upgrade().is_none() {
                                    // All write ends closed — EOF
                                    tf.regs[10] = 0;
                                } else {
                                    // Block until the write half wakes us — do NOT spin.
                                    let waiter = half.waiter.clone();
                                    drop(data);
                                    drop(fds);
                                    drop(proc);
                                    tf.sepc -= 4;
                                    waiter.store(
                                        crate::posix::process::current_pid(),
                                        Ordering::Relaxed,
                                    );
                                    crate::posix::process::schedule();
                                    unsafe { __halt_cpu() }
                                }
                            }
                            _ => {
                                tf.regs[10] = usize::MAX; // EBADF
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // Error
                    }
                }
                6 => {
                    // sys_spawn
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::posix::spawn::posix_spawn(
                            path,
                            crate::posix::process::current_pid(),
                        ) {
                            Ok(child_pid) => tf.regs[10] = child_pid as usize,
                            Err(e) => {
                                crate::println!("sys_spawn: {}: {}", path, e);
                                tf.regs[10] = usize::MAX;
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                8 => {
                    // sys_open
                    // arg0 = path_ptr, arg1 = path_len, arg2 = flags (O_RDONLY=0, O_WRONLY=1,
                    //   O_RDWR=2; O_CLOEXEC=0x80000)
                    const O_CLOEXEC: usize = 0x80000;
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        if let Ok(vnode) = crate::vfs::lookup_path(path) {
                            let proc = get_current_proc_or_esrch!(tf);

                            // Basic flag decoding: 0=O_RDONLY, 1=O_WRONLY, 2=O_RDWR
                            let mut requested = 0;
                            if (arg2 & 3) == 0 || (arg2 & 3) == 2 {
                                requested |= crate::vfs::ACCESS_READ;
                            }
                            if (arg2 & 3) == 1 || (arg2 & 3) == 2 {
                                requested |= crate::vfs::ACCESS_WRITE;
                            }

                            if crate::vfs::check_access(&proc, &*vnode, requested) {
                                let fd_desc = crate::ipc::handle::FileDescription {
                                    vnode,
                                    offset: crate::sync::Mutex::new(0),
                                };
                                let mut fds = proc.fds.lock();
                                let fd = fds.insert(crate::ipc::handle::KernelObject::File(
                                    alloc::sync::Arc::new(fd_desc),
                                ));
                                if (arg2 & O_CLOEXEC) != 0 {
                                    fds.set_cloexec(fd, true);
                                }
                                tf.regs[10] = fd as usize;
                            } else {
                                crate::println!("sys_open: access denied to '{}'", path);
                                tf.regs[10] = usize::MAX - 12; // EACCES
                            }
                        } else {
                            tf.regs[10] = usize::MAX; // ENOENT
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // EINVAL
                    }
                }
                9 => {
                    // sys_close
                    // arg0 = fd
                    let proc = get_current_proc_or_esrch!(tf);
                    if proc.fds.lock().remove(arg0 as u32).is_some() {
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX; // EBADF
                    }
                }
                12 => {
                    // sys_getdents — arg0=path_ptr, arg1=path_len, arg2=buf_ptr, a3=buf_len
                    let buf_len = tf.regs[13]; // a3
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        if let Ok(vnode) = crate::vfs::lookup_path(path) {
                            match vnode.readdir() {
                                Ok(entries) => {
                                    let buf = unsafe {
                                        core::slice::from_raw_parts_mut(arg2 as *mut u8, buf_len)
                                    };
                                    let mut pos = 0;
                                    for entry in entries {
                                        let name_bytes = entry.name.as_bytes();
                                        if pos + name_bytes.len() + 1 > buf_len {
                                            break;
                                        }
                                        buf[pos..pos + name_bytes.len()]
                                            .copy_from_slice(name_bytes);
                                        pos += name_bytes.len();
                                        buf[pos] = 0;
                                        pos += 1;
                                    }
                                    tf.regs[10] = pos;
                                }
                                Err(_) => tf.regs[10] = usize::MAX,
                            }
                        } else {
                            tf.regs[10] = usize::MAX;
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                10 => {
                    // sys_net_send
                    // arg0 = ptr, arg1 = len
                    let pkt = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Some(dev) = crate::net::device() {
                        dev.send(pkt);
                        tf.regs[10] = arg1; // bytes sent
                    } else {
                        tf.regs[10] = usize::MAX; // ENODEV
                    }
                }
                11 => {
                    // sys_net_recv
                    // arg0 = buf ptr, arg1 = max_len
                    let buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1) };
                    if let Some(dev) = crate::net::device() {
                        let got = dev.recv(buf);
                        tf.regs[10] = got;
                    } else {
                        tf.regs[10] = usize::MAX; // ENODEV
                    }
                }
                40 => {
                    // sys_socket -- create a proxied socket: pair a channel with net-daemon
                    // returns a user fd backed by a Channel; the net-daemon will pick up the peer via sys_daemon_next_socket
                    let (ep_user, ep_net) = crate::ipc::channel::ChannelEndpoint::create_pair();
                    let proc = get_current_proc_or_esrch!(tf);
                    let user_fd = proc
                        .fds
                        .lock()
                        .insert(crate::ipc::handle::KernelObject::Channel(ep_user));

                    // Allocate socket id and register ownership
                    let sid = crate::net::socket::alloc_socket_id();
                    crate::net::socket::register_socket_owner(
                        sid,
                        crate::posix::process::current_pid(),
                        user_fd,
                    );
                    crate::net::socket::push_new_socket(sid, ep_net);

                    tf.regs[10] = user_fd as usize;
                }
                41 => {
                    // sys_daemon_next_socket -- net-daemon pulls next socket created by a user
                    if let Some((sid, ep)) = crate::net::socket::pop_new_socket() {
                        let proc = get_current_proc_or_esrch!(tf);
                        let fd = proc
                            .fds
                            .lock()
                            .insert(crate::ipc::handle::KernelObject::Channel(ep));
                        // pack sid (high 32) and fd (low 32) into return value
                        let ret = ((sid as usize) << 32) | (fd as usize);
                        tf.regs[10] = ret;
                    } else {
                        tf.regs[10] = usize::MAX; // no socket available
                    }
                }
                42 => {
                    // sys_daemon_create_conn(listen_sid)
                    // arg0 = listen socket id
                    let listen_sid = arg0 as u32;
                    if let Some((owner_pid, _owner_fd)) = crate::net::socket::owner_of(listen_sid) {
                        // Create a channel pair to represent the accepted connection
                        let (user_ep, net_ep) = crate::ipc::channel::ChannelEndpoint::create_pair();

                        // Insert net_ep into daemon's fd table (return to daemon)
                        let proc = get_current_proc_or_esrch!(tf);
                        let net_fd = proc
                            .fds
                            .lock()
                            .insert(crate::ipc::handle::KernelObject::Channel(net_ep));

                        // Deliver user_ep into the owner process: either wake a blocked accept or queue it
                        crate::net::socket::push_pending_accepted(listen_sid, {
                            // insert into owner's fd table
                            let owner_proc = match crate::posix::process::PROCESS_TABLE
                                .lock()
                                .get(&owner_pid)
                                .cloned()
                            {
                                Some(p) => p,
                                None => {
                                    tf.regs[10] = crate::errno::ESRCH;
                                    tf.sepc += 4;
                                    return tf as *mut _;
                                }
                            };
                            owner_proc
                                .fds
                                .lock()
                                .insert(crate::ipc::handle::KernelObject::Channel(user_ep))
                        });

                        // If the owner is currently blocked in accept, pop the waiting pid and wake it
                        if let Some(waiting_pid) = crate::net::socket::pop_waiting_pid(listen_sid) {
                            // Wake the waiting pid: ensure it's scheduled to run; the accept syscall will check pending_accepted
                            crate::posix::process::RUN_QUEUE
                                .lock()
                                .push_back(waiting_pid);
                        }

                        tf.regs[10] = net_fd as usize;
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                43 => {
                    // sys_accept (userland)
                    // arg0 = listen_fd
                    let listen_fd = arg0 as u32;
                    // Find socket id for this fd
                    if let Some(sid) = crate::net::socket::socket_id_for_fd(
                        crate::posix::process::current_pid(),
                        listen_fd,
                    ) {
                        // If there's a pending accepted fd, return it
                        if let Some(fd) = crate::net::socket::pop_pending_accepted(sid) {
                            tf.regs[10] = fd as usize;
                        } else {
                            // Block and add to pending accepts
                            crate::net::socket::add_pending_accept(
                                sid,
                                crate::posix::process::current_pid(),
                            );
                            tf.sepc -= 4; // rewind ecall
                            crate::posix::process::RUN_QUEUE
                                .lock()
                                .push_back(crate::posix::process::current_pid());
                            crate::posix::process::schedule();
                            unsafe { __halt_cpu() }
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                44 => {
                    // sys_bind(fd, addr_ptr, addr_len)
                    let fd = arg0 as u32;
                    let addr_ptr = arg1 as *const u8;
                    let addr_len = arg2 as usize;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                // Copy addr bytes
                                let addr_bytes =
                                    unsafe { core::slice::from_raw_parts(addr_ptr, addr_len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_BIND);
                                v.extend_from_slice(addr_bytes);
                                let msg = crate::ipc::channel::Message {
                                    bytes: v,
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX
                    }
                }
                45 => {
                    // sys_listen(fd, backlog, _)
                    let fd = arg0 as u32;
                    let backlog = arg1 as u32;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_LISTEN);
                                v.extend_from_slice(&backlog.to_le_bytes());
                                let msg = crate::ipc::channel::Message {
                                    bytes: v,
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX
                    }
                }
                46 => {
                    // sys_connect(fd, addr_ptr, addr_len)
                    let fd = arg0 as u32;
                    let addr_ptr = arg1 as *const u8;
                    let addr_len = arg2 as usize;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let addr_bytes =
                                    unsafe { core::slice::from_raw_parts(addr_ptr, addr_len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_CONNECT);
                                v.extend_from_slice(addr_bytes);
                                let msg = crate::ipc::channel::Message {
                                    bytes: v,
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX
                    }
                }
                47 => {
                    // sys_sock_send(fd, buf_ptr, len)
                    let fd = arg0 as u32;
                    let buf_ptr = arg1 as *const u8;
                    let len = arg2 as usize;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let data = unsafe { core::slice::from_raw_parts(buf_ptr, len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_SEND);
                                v.extend_from_slice(data);
                                let msg = crate::ipc::channel::Message {
                                    bytes: v,
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = len,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX
                    }
                }
                48 => {
                    // sys_sock_recv(fd, buf_ptr, max_len)
                    let fd = arg0 as u32;
                    let buf_ptr = arg1 as *mut u8;
                    let max_len = arg2 as usize;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                if let Some(msg) = ep.try_recv() {
                                    let to_copy = core::cmp::min(msg.bytes.len(), max_len);
                                    let dst = unsafe {
                                        core::slice::from_raw_parts_mut(buf_ptr, to_copy)
                                    };
                                    dst.copy_from_slice(&msg.bytes[..to_copy]);
                                    tf.regs[10] = to_copy;
                                } else {
                                    // Block until message arrives
                                    drop(fds);
                                    drop(proc);
                                    tf.sepc -= 4;
                                    crate::posix::process::RUN_QUEUE
                                        .lock()
                                        .push_back(crate::posix::process::current_pid());
                                    crate::posix::process::schedule();
                                    unsafe { __halt_cpu() }
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX
                    }
                }
                49 => {
                    // sys_try_recv(fd, buf_ptr, max_len) — non-blocking Channel read
                    // Returns bytes read, or 0 if no message is pending (never sleeps).
                    let fd = arg0 as u32;
                    let buf_ptr = arg1 as *mut u8;
                    let max_len = arg2 as usize;
                    let proc = get_current_proc_or_esrch!(tf);
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                if let Some(msg) = ep.try_recv() {
                                    let to_copy = core::cmp::min(msg.bytes.len(), max_len);
                                    let dst = unsafe {
                                        core::slice::from_raw_parts_mut(buf_ptr, to_copy)
                                    };
                                    dst.copy_from_slice(&msg.bytes[..to_copy]);
                                    tf.regs[10] = to_copy;
                                } else {
                                    tf.regs[10] = 0; // no data, return 0 (no block)
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                23 => {
                    // sys_setuid
                    let proc = get_current_proc_or_esrch!(tf);
                    if proc.euid.load(Ordering::Relaxed) == 0 {
                        // Root can set UID to anything
                        let val = arg0 as u32;
                        proc.uid.store(val, Ordering::Relaxed);
                        proc.euid.store(val, Ordering::Relaxed);
                        tf.regs[10] = 0;
                    } else if arg0 as u32 == proc.uid.load(Ordering::Relaxed) {
                        // Non-root can set euid back to real uid
                        proc.euid.store(arg0 as u32, Ordering::Relaxed);
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX - 1; // EPERM
                    }
                }
                24 => {
                    // sys_setgid
                    let proc = get_current_proc_or_esrch!(tf);
                    if proc.euid.load(Ordering::Relaxed) == 0 {
                        let val = arg0 as u32;
                        proc.gid.store(val, Ordering::Relaxed);
                        proc.egid.store(val, Ordering::Relaxed);
                        tf.regs[10] = 0;
                    } else if arg0 as u32 == proc.gid.load(Ordering::Relaxed) {
                        proc.egid.store(arg0 as u32, Ordering::Relaxed);
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX - 1; // EPERM
                    }
                }
                26 => {
                    // sys_create — create or truncate a file, return writable fd
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::vfs::lookup_parent(path) {
                            Ok((parent, name)) => match parent.create(&name) {
                                Ok(vnode) => {
                                    let proc = get_current_proc_or_esrch!(tf);
                                    let fd_desc = crate::ipc::handle::FileDescription {
                                        vnode,
                                        offset: crate::sync::Mutex::new(0),
                                    };
                                    let fd = proc.fds.lock().insert(
                                        crate::ipc::handle::KernelObject::File(
                                            alloc::sync::Arc::new(fd_desc),
                                        ),
                                    );
                                    tf.regs[10] = fd as usize;
                                }
                                Err(_) => tf.regs[10] = usize::MAX,
                            },
                            Err(_) => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                30 => {
                    // sys_getuid
                    let proc = get_current_proc_or_esrch!(tf);
                    tf.regs[10] = proc.uid.load(Ordering::Relaxed) as usize;
                }
                31 => {
                    // sys_geteuid
                    let proc = get_current_proc_or_esrch!(tf);
                    tf.regs[10] = proc.euid.load(Ordering::Relaxed) as usize;
                }
                32 => {
                    // sys_getgid
                    let proc = get_current_proc_or_esrch!(tf);
                    tf.regs[10] = proc.gid.load(Ordering::Relaxed) as usize;
                }
                33 => {
                    // sys_getegid
                    let proc = get_current_proc_or_esrch!(tf);
                    tf.regs[10] = proc.egid.load(Ordering::Relaxed) as usize;
                }
                34 => {
                    // sys_authenticate
                    // arg0=user_ptr, arg1=user_len, arg2=pass_ptr, a3=pass_len
                    let pass_len = tf.regs[13]; // a3
                    let username_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    let password_bytes =
                        unsafe { core::slice::from_raw_parts(arg2 as *const u8, pass_len) };
                    if let Ok(username) = core::str::from_utf8(username_bytes) {
                        match crate::posix::user::authenticate(username, password_bytes) {
                            Some(entry) => tf.regs[10] = entry.uid as usize,
                            None => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                35 => {
                    // sys_can_sudo
                    // arg0 = uid to check (or 0 = use current process uid)
                    let uid = if arg0 == 0 {
                        let proc = get_current_proc_or_esrch!(tf);
                        proc.uid.load(Ordering::Relaxed)
                    } else {
                        arg0 as u32
                    };
                    tf.regs[10] = if crate::posix::user::can_sudo(uid) {
                        1
                    } else {
                        0
                    };
                }
                37 => {
                    // sys_set_echo — arg0: 0=disable, 1=enable
                    *ECHO_ENABLED.lock() = arg0 != 0;
                    tf.regs[10] = 0;
                }
                38 => {
                    // sys_set_raw — arg0: 1=raw (char-by-char), 0=cooked (line discipline)
                    *RAW_MODE.lock() = arg0 != 0;
                    tf.regs[10] = 0;
                }
                50 => {
                    // sys_fork (scaffold -> COW)
                    match crate::posix::spawn::sys_fork() {
                        Ok(child_pid) => {
                            // In parent, return child's pid
                            tf.regs[10] = child_pid as usize;
                        }
                        Err(_) => {
                            tf.regs[10] = usize::MAX; // error for now
                        }
                    }
                }
                51 => {
                    // sys_exec(path_ptr, path_len, argv_buf_ptr, argv_buf_len)
                    // argv_buf: packed null-terminated strings "arg0\0arg1\0...argN\0"
                    let path_ptr = arg0 as *const u8;
                    let path_len = arg1 as usize;
                    let argv_buf_ptr = arg2 as *const u8;
                    let argv_buf_len = tf.regs[13]; // a3

                    // Copy argv from user memory BEFORE sys_exec destroys the old address space.
                    let argv_data: alloc::vec::Vec<u8> = if argv_buf_len > 0 && arg2 != 0 {
                        unsafe { core::slice::from_raw_parts(argv_buf_ptr, argv_buf_len).to_vec() }
                    } else {
                        alloc::vec::Vec::new()
                    };

                    match crate::posix::spawn::sys_exec(path_ptr, path_len) {
                        Ok(entry_point) => {
                            let pid = crate::posix::process::current_pid();

                            // Close FD_CLOEXEC handles — the new image must not see them.
                            {
                                let table = crate::posix::process::PROCESS_TABLE.lock();
                                if let Some(proc) = table.get(&pid) {
                                    proc.fds.lock().close_on_exec();
                                }
                            }

                            // Reset wait state — new image starts with no pending wait.
                            {
                                let table = crate::posix::process::PROCESS_TABLE.lock();
                                if let Some(proc) = table.get(&pid) {
                                    *proc.wait_result.lock() = None;
                                    *proc.wait_target.lock() = None;
                                }
                            }

                            // Set up argc/argv on the new user stack.
                            let (argc, argv_ptr, new_sp) = setup_exec_stack(&argv_data);

                            // Reinstall the new page table on this CPU (sys_exec already
                            // stored it in proc.satp_val; we need the CPU to see it).
                            let new_satp = crate::posix::process::PROCESS_TABLE
                                .lock()
                                .get(&pid)
                                .map(|p| p.satp_val.load(Ordering::Relaxed))
                                .unwrap_or(0);
                            unsafe {
                                riscv::register::satp::write(new_satp);
                                core::arch::asm!("sfence.vma");
                            }

                            tf.sepc = entry_point;
                            tf.regs[2] = new_sp; // sp
                            tf.regs[10] = argc; // a0 = argc
                            tf.regs[11] = argv_ptr; // a1 = argv
                            tf.sstatus = (1 << 5) | (1 << 18);
                            return tf as *mut _;
                        }
                        Err(e) => {
                            crate::println!("sys_exec error: {}", e);
                            tf.regs[10] = usize::MAX;
                        }
                    }
                }
                52 => {
                    // sys_waitpid(target_pid, status_ptr, options)
                    let target = arg0 as i32; // pid or -1 for any
                    let status_ptr = arg1 as *mut i32;
                    let ppid = crate::posix::process::current_pid();

                    // Step 1: consume a pre-delivered wait_result (lock/take/release).
                    let delivered = {
                        let table = crate::posix::process::PROCESS_TABLE.lock();
                        table.get(&ppid).and_then(|p| p.wait_result.lock().take())
                    }; // PROCESS_TABLE lock dropped here

                    if let Some((zpid, zstatus)) = delivered {
                        // Extract resources then remove — must free after lock is released.
                        let (kstack, zombie_satp, parent_arc) = {
                            let mut table = crate::posix::process::PROCESS_TABLE.lock();
                            let r = table
                                .get(&zpid)
                                .map(|p| {
                                    (p.kernel_stack_bottom, p.satp_val.load(Ordering::Relaxed))
                                })
                                .unwrap_or((0, 0));
                            table.remove(&zpid);
                            let parent = table.get(&ppid).cloned();
                            (r.0, r.1, parent)
                        };
                        if let Some(parent) = parent_arc {
                            parent.children.lock().retain(|&p| p != zpid);
                            *parent.wait_target.lock() = None;
                            *parent.wait_status_ptr.lock() = None;
                        }
                        // Free resources after lock is dropped.
                        if kstack != 0 {
                            const KSZ: usize = 65536;
                            unsafe {
                                alloc::alloc::dealloc(
                                    kstack as *mut u8,
                                    core::alloc::Layout::from_size_align(KSZ, 16)
                                        .unwrap_or_else(|_| unreachable!("constant layout")),
                                );
                            }
                        }
                        let root_pa = (zombie_satp & 0xFFFFFFFFFFF) << 12;
                        if root_pa != 0 {
                            let _ = crate::mm::vmm::destroy_user_space(root_pa);
                        }
                        if !status_ptr.is_null() {
                            unsafe {
                                *status_ptr = zstatus;
                            }
                        }
                        tf.regs[10] = zpid as usize;
                    } else {
                        // Step 2: scan children for an existing zombie.
                        let found_zombie = {
                            let table = crate::posix::process::PROCESS_TABLE.lock();
                            let mut found = None;
                            if let Some(parent) = table.get(&ppid) {
                                let children: alloc::vec::Vec<_> = parent.children.lock().clone();
                                for child_pid in children {
                                    if target == -1 || target == child_pid {
                                        if let Some(child) = table.get(&child_pid) {
                                            if let crate::posix::process::ProcState::Zombie(st) =
                                                *child.state.lock()
                                            {
                                                found = Some((child_pid, st));
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            found
                        }; // PROCESS_TABLE lock dropped here

                        if let Some((zpid, zstatus)) = found_zombie {
                            // Extract resources then remove.
                            let (kstack, zombie_satp, parent_arc) = {
                                let mut table = crate::posix::process::PROCESS_TABLE.lock();
                                let r = table
                                    .get(&zpid)
                                    .map(|p| {
                                        (p.kernel_stack_bottom, p.satp_val.load(Ordering::Relaxed))
                                    })
                                    .unwrap_or((0, 0));
                                table.remove(&zpid);
                                let parent = table.get(&ppid).cloned();
                                (r.0, r.1, parent)
                            };
                            if let Some(parent) = parent_arc {
                                parent.children.lock().retain(|&p| p != zpid);
                                *parent.wait_target.lock() = None;
                                *parent.wait_status_ptr.lock() = None;
                            }
                            if kstack != 0 {
                                const KSZ: usize = 65536;
                                unsafe {
                                    alloc::alloc::dealloc(
                                        kstack as *mut u8,
                                        core::alloc::Layout::from_size_align(KSZ, 16)
                                            .unwrap_or_else(|_| unreachable!("constant layout")),
                                    );
                                }
                            }
                            let root_pa = (zombie_satp & 0xFFFFFFFFFFF) << 12;
                            if root_pa != 0 {
                                let _ = crate::mm::vmm::destroy_user_space(root_pa);
                            }
                            if !status_ptr.is_null() {
                                unsafe {
                                    *status_ptr = zstatus;
                                }
                            }
                            tf.regs[10] = zpid as usize;
                        } else {
                            // Step 3: block — acquire the process Arc, release PT lock,
                            // then update state and yield.
                            let proc_arc = crate::posix::process::PROCESS_TABLE
                                .lock()
                                .get(&ppid)
                                .cloned();
                            if let Some(proc) = proc_arc {
                                // PROCESS_TABLE lock is already released (cloned Arc).
                                *proc.wait_target.lock() = Some(target);
                                *proc.wait_status_ptr.lock() = Some(status_ptr as usize);
                                tf.sepc -= 4; // restart waitpid when resumed
                                *proc.state.lock() = crate::posix::process::ProcState::Stopped;
                                // Do NOT push to RUN_QUEUE — exit() will set us Running
                                // and re-queue us when the child exits.
                                drop(proc); // drop Arc before diverging into schedule()
                                crate::posix::process::schedule();
                                unsafe { __halt_cpu() }
                            } else {
                                tf.regs[10] = usize::MAX; // parent not found
                            }
                        }
                    }
                }
                27 => {
                    // sys_mkdir(path_ptr, path_len)
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::vfs::lookup_parent(path) {
                            Ok((parent, name)) => match parent.mkdir(&name) {
                                Ok(_) => tf.regs[10] = 0,
                                Err(_) => tf.regs[10] = usize::MAX,
                            },
                            Err(_) => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                28 => {
                    // sys_unlink(path_ptr, path_len)
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::vfs::lookup_parent(path) {
                            Ok((parent, name)) => match parent.unlink(&name) {
                                Ok(()) => tf.regs[10] = 0,
                                Err(_) => tf.regs[10] = usize::MAX,
                            },
                            Err(_) => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                29 => {
                    // sys_rename(old_ptr, old_len, new_ptr, new_len)
                    let new_ptr = arg2;
                    let new_len = tf.regs[13]; // a3
                    let old_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    let new_bytes =
                        unsafe { core::slice::from_raw_parts(new_ptr as *const u8, new_len) };
                    if let (Ok(old_path), Ok(new_path)) = (
                        core::str::from_utf8(old_bytes),
                        core::str::from_utf8(new_bytes),
                    ) {
                        match crate::vfs::lookup_parent(old_path) {
                            Ok((old_parent, old_name)) => {
                                match crate::vfs::lookup_parent(new_path) {
                                    Ok((new_parent, new_name)) => {
                                        if alloc::sync::Arc::ptr_eq(&old_parent, &new_parent) {
                                            match old_parent.rename(&old_name, &new_name) {
                                                Ok(()) => tf.regs[10] = 0,
                                                Err(_) => tf.regs[10] = usize::MAX,
                                            }
                                        } else {
                                            // Cross-directory rename not yet supported
                                            // by the Vnode trait.
                                            tf.regs[10] = usize::MAX;
                                        }
                                    }
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            Err(_) => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                53 => {
                    // sys_getpid
                    tf.regs[10] = crate::posix::process::current_pid() as usize;
                }
                54 => {
                    // sys_getppid
                    let ppid = crate::posix::process::PROCESS_TABLE
                        .lock()
                        .get(&crate::posix::process::current_pid())
                        .map(|p| p.ppid.load(core::sync::atomic::Ordering::Relaxed))
                        .unwrap_or(0);
                    tf.regs[10] = ppid as usize;
                }
                55 => {
                    // sys_chdir(path_ptr, path_len)
                    let path_bytes =
                        unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::vfs::lookup_path(path) {
                            Ok(vnode) if vnode.stat().vtype == crate::vfs::VnodeType::Directory => {
                                let proc = get_current_proc_or_esrch!(tf);
                                *proc.cwd.lock() = alloc::string::String::from(path);
                                tf.regs[10] = 0;
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                56 => {
                    // sys_getcwd(buf_ptr, buf_len)
                    let proc = get_current_proc_or_esrch!(tf);
                    let cwd = proc.cwd.lock().clone();
                    let bytes = cwd.as_bytes();
                    let len = core::cmp::min(arg1, bytes.len());
                    let user_buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, len) };
                    user_buf.copy_from_slice(&bytes[..len]);
                    tf.regs[10] = len;
                }
                57 => {
                    // sys_dup(oldfd) → new_fd (lowest available)
                    let proc = get_current_proc_or_esrch!(tf);
                    match proc.fds.lock().dup(arg0 as u32) {
                        Some(new_h) => tf.regs[10] = new_h as usize,
                        None => tf.regs[10] = usize::MAX,
                    }
                }
                58 => {
                    // sys_dup2(oldfd, newfd) → newfd
                    let proc = get_current_proc_or_esrch!(tf);
                    if proc.fds.lock().dup2(arg0 as u32, arg1 as u32) {
                        tf.regs[10] = arg1;
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                60 => {
                    // sys_set_fg_pid(pid) — register foreground process for SIGINT delivery
                    let pid = arg0 as i32;
                    crate::posix::process::FOREGROUND_PID.store(pid, Ordering::Relaxed);
                    tf.regs[10] = 0;
                }
                61 => {
                    // sys_ipc_send(to_pid, buf_ptr, buf_len) → 0 or -1
                    let to_pid = arg0 as i32;
                    let buf_ptr = arg1;
                    let buf_len = arg2;

                    if buf_len > 4096 {
                        tf.regs[10] = usize::MAX; // message too large
                    } else {
                        let data = unsafe {
                            core::slice::from_raw_parts(buf_ptr as *const u8, buf_len).to_vec()
                        };
                        let sender = crate::posix::process::current_pid();
                        let target = crate::posix::process::PROCESS_TABLE
                            .lock()
                            .get(&to_pid)
                            .cloned();
                        if let Some(tp) = target {
                            tp.mailbox.lock().push_back((sender, data));
                            // Wake if blocked in sys_ipc_recv.
                            let w = tp.mailbox_waiter.swap(0, Ordering::Relaxed);
                            if w != 0 {
                                crate::posix::process::RUN_QUEUE.lock().push_back(to_pid);
                            }
                            tf.regs[10] = 0;
                        } else {
                            tf.regs[10] = usize::MAX; // no such pid
                        }
                    }
                }
                62 => {
                    // sys_ipc_recv(buf_ptr, max_len, from_pid_ptr) → bytes_received
                    let buf_ptr = arg0;
                    let max_len = arg1;
                    let from_pid_ptr = arg2;

                    let pid = crate::posix::process::current_pid();
                    let msg = crate::posix::process::PROCESS_TABLE
                        .lock()
                        .get(&pid)
                        .and_then(|p| p.mailbox.lock().pop_front());

                    if let Some((sender, data)) = msg {
                        let len = core::cmp::min(max_len, data.len());
                        unsafe {
                            core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len)
                                .copy_from_slice(&data[..len]);
                            if from_pid_ptr != 0 {
                                *(from_pid_ptr as *mut i32) = sender;
                            }
                        }
                        tf.regs[10] = len;
                    } else {
                        // Block — set waiter flag and rewind ecall so it retries on wakeup.
                        tf.sepc -= 4;
                        if let Some(p) = crate::posix::process::PROCESS_TABLE.lock().get(&pid) {
                            p.mailbox_waiter.store(pid, Ordering::Relaxed);
                        }
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
                63 => {
                    // sys_vfs_register() — caller becomes the global VFS server
                    let pid = crate::posix::process::current_pid();
                    VFS_SERVER_PID.store(pid, Ordering::Relaxed);
                    crate::println!("VFS server registered: pid {}", pid);
                    tf.regs[10] = 0;
                }
                64 => {
                    // sys_get_vfs_pid() → VFS server pid, or -1 if not registered
                    let pid = VFS_SERVER_PID.load(Ordering::Relaxed);
                    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
                }
                65 => {
                    // sys_initrd_data(buf_ptr, offset, max_len) → bytes_copied
                    // Lets the VFS server fetch chunks of the initrd TAR from kernel memory.
                    let buf_ptr = arg0;
                    let offset = arg1;
                    let max_len = arg2;
                    let start = crate::INITRD_START.load(Ordering::Relaxed);
                    let total = crate::INITRD_LEN.load(Ordering::Relaxed);
                    if start == 0 || offset >= total {
                        tf.regs[10] = 0;
                    } else {
                        let avail = total - offset;
                        let copy_len = max_len.min(avail).min(4096);
                        unsafe {
                            let src = core::slice::from_raw_parts(
                                (start + offset) as *const u8,
                                copy_len,
                            );
                            let dst = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, copy_len);
                            dst.copy_from_slice(src);
                        }
                        tf.regs[10] = copy_len;
                    }
                }
                _ => {
                    // Unknown syscall — return ENOSYS rather than crashing the kernel.
                    crate::println!(
                        "pid {}: unknown syscall {}",
                        crate::posix::process::current_pid(),
                        syscall_num
                    );
                    tf.regs[10] = usize::MAX; // -1 (ENOSYS)
                }
            }

            tf as *mut _
        }
        // Instruction or load page fault — demand paging
        scause::Trap::Exception(12) | scause::Trap::Exception(13) => {
            use riscv::register::stval;
            let fault_va = stval::read();
            match crate::mm::vmm::handle_user_page_fault(fault_va) {
                Ok(()) => tf as *mut _,
                Err(e) => {
                    let pid = crate::posix::process::current_pid();
                    crate::println!("Segfault pid {}: {} at va={:#x}", pid, e, fault_va);
                    crate::posix::spawn::exit(pid, -11);
                    crate::posix::process::schedule();
                    unsafe { __halt_cpu() }
                }
            }
        }
        // Store/AMO page fault — copy-on-write, then demand paging (stack growth)
        scause::Trap::Exception(15) => {
            use riscv::register::stval;
            let fault_va = stval::read();
            // Try COW first (fork: shared read-only page → private writable copy).
            match crate::mm::vmm::handle_store_page_fault(fault_va) {
                Ok(()) => tf as *mut _,
                Err(_) => {
                    // Not a COW page — try demand paging (stack growth or lazy alloc).
                    match crate::mm::vmm::handle_user_page_fault(fault_va) {
                        Ok(()) => tf as *mut _,
                        Err(e) => {
                            let pid = crate::posix::process::current_pid();
                            crate::println!(
                                "Segfault pid {}: store {} at va={:#x}",
                                pid,
                                e,
                                fault_va
                            );
                            crate::posix::spawn::exit(pid, -11);
                            crate::posix::process::schedule();
                            unsafe { __halt_cpu() }
                        }
                    }
                }
            }
        }
        // Supervisor timer interrupt — preempt current process.
        // Timer fires only in user mode (SIE=1), so no kernel locks are held;
        // calling schedule() here is safe.
        scause::Trap::Interrupt(5) => {
            crate::timer::set_next_timer();

            // Drain all available UART bytes into LINE_DISC_BUFFER in one shot.
            // Only HART 0 polls UART to avoid multiple HARTs racing on the RX register.
            if crate::smp::current_hartid() == 0 {
                poll_uart_into_linedisc();
            }

            // Only preempt if we came from U-mode (SPP == 0) AND there is genuinely
            // another runnable process.  Do NOT push the current pid when staying put —
            // that would create a phantom queue entry and defeat the sys_yield no-op.
            let is_user = (tf.sstatus & (1 << 8)) == 0;
            if is_user {
                let pid = crate::posix::process::current_pid();
                let should_preempt = !crate::posix::process::RUN_QUEUE.lock().is_empty();
                if should_preempt {
                    if pid != 0 {
                        crate::posix::process::RUN_QUEUE.lock().push_back(pid);
                    }
                    crate::posix::process::schedule();
                    unsafe { __halt_cpu() }
                } else {
                    tf as *mut _
                }
            } else {
                tf as *mut _
            }
        }
        // Supervisor software interrupt — TLB shootdown IPI
        scause::Trap::Interrupt(1) => {
            unsafe {
                core::arch::asm!("sfence.vma");
                core::arch::asm!("csrci sip, 2"); // clear SSIP
            }
            tf as *mut _
        }
        // External interrupts — PLIC claim/complete
        scause::Trap::Interrupt(9) | scause::Trap::Interrupt(11) => {
            let hart = crate::smp::current_hartid();
            let irq = crate::plic::claim(hart as usize);
            if irq != 0 {
                crate::drivers::dispatch_interrupt(irq as usize);
                crate::plic::complete(hart as usize, irq);
            }
            tf as *mut _
        }
        scause::Trap::Interrupt(n) => {
            crate::println!("Unhandled interrupt: {}", n);
            tf as *mut _
        }
        // Instruction address misaligned
        scause::Trap::Exception(0) => {
            if riscv::register::sstatus::read().spp() == riscv::register::sstatus::SPP::Supervisor {
                panic!("Kernel instruction address misaligned at sepc={:#x}", sepc);
            }
            let pid = crate::posix::process::current_pid();
            crate::println!(
                "pid {}: instruction address misaligned sepc={:#x} — killed",
                pid,
                sepc
            );
            crate::posix::spawn::exit(pid, -4); // SIGILL equivalent
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        // Illegal instruction
        scause::Trap::Exception(2) => {
            if riscv::register::sstatus::read().spp() == riscv::register::sstatus::SPP::Supervisor {
                panic!(
                    "Kernel illegal instruction at sepc={:#x} stval={:#x}",
                    sepc, stval
                );
            }
            let pid = crate::posix::process::current_pid();
            crate::println!(
                "pid {}: illegal instruction sepc={:#x} stval={:#x} — killed",
                pid,
                sepc,
                stval
            );
            crate::posix::spawn::exit(pid, -4); // SIGILL
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        // Load/Store address misaligned
        scause::Trap::Exception(4) | scause::Trap::Exception(6) => {
            if riscv::register::sstatus::read().spp() == riscv::register::sstatus::SPP::Supervisor {
                panic!(
                    "Kernel misaligned memory access at sepc={:#x} stval={:#x}",
                    sepc, stval
                );
            }
            let pid = crate::posix::process::current_pid();
            crate::println!(
                "pid {}: misaligned memory access sepc={:#x} stval={:#x} — killed",
                pid,
                sepc,
                stval
            );
            crate::posix::spawn::exit(pid, -7); // SIGBUS
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        _ => {
            // Unhandled exception
            if riscv::register::sstatus::read().spp() == riscv::register::sstatus::SPP::Supervisor {
                panic!(
                    "Kernel unhandled trap {:?} at sepc={:#x} stval={:#x}",
                    cause, sepc, stval
                );
            }
            let pid = crate::posix::process::current_pid();
            crate::println!(
                "pid {}: unhandled trap {:?} sepc={:#x} stval={:#x} — killed",
                pid,
                cause,
                tf.sepc,
                stval
            );
            crate::posix::spawn::exit(pid, -1);
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
    }
}
