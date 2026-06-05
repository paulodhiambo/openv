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

pub static LINE_DISC_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
pub static ECHO_ENABLED: Mutex<bool> = Mutex::new(true);
pub static RAW_MODE: Mutex<bool> = Mutex::new(false);
pub static STDIN_WAITER: AtomicI32 = AtomicI32::new(0);
pub static CTRLC_PENDING: AtomicBool = AtomicBool::new(false);


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

    let ret_tf = match cause {
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

            let arg3 = tf.regs[13];
            crate::syscall::dispatch(syscall_num, arg0, arg1, arg2, arg3, tf);
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
            crate::posix::process::wake_sleepers();

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
    };

    // If we are returning to user-space (SPP == 0), check for pending signals
    unsafe {
        let mut_tf = &mut *(ret_tf as *mut TrapFrame);
        if (mut_tf.sstatus & (1 << 8)) == 0 {
            let pid = crate::posix::process::current_pid();
            let table = crate::posix::process::PROCESS_TABLE.lock();
            if let Some(proc) = table.get(&pid) {
                let pending = proc.pending_signals.load(core::sync::atomic::Ordering::Relaxed);
                let blocked = proc.blocked_signals.load(core::sync::atomic::Ordering::Relaxed);
                let deliverable = pending & !blocked;
                
                if deliverable != 0 {
                    // Find first set bit (the signal number)
                    let sig = deliverable.trailing_zeros();
                    
                    // Clear the pending bit
                    proc.pending_signals.fetch_and(!(1 << sig), core::sync::atomic::Ordering::Relaxed);
                    
                    if sig == crate::syscall::proc::SIGKILL as u32 {
                        // Force exit immediately
                        drop(table);
                        crate::posix::spawn::exit(pid, -9);
                        crate::posix::process::schedule();
                        __halt_cpu()
                    } else {
                        let handlers = proc.signal_handlers.lock();
                        let handler_addr = handlers[sig as usize];
                        
                        // If no handler is registered, default action is typically to terminate (except ignored ones)
                        // We'll just terminate for now if there's no handler, except for SIGCHLD which we ignore (0 is default).
                        // Actually, let's keep it simple: if handler_addr == 0, we kill the process.
                        if handler_addr == 0 {
                            drop(handlers);
                            drop(table);
                            crate::posix::spawn::exit(pid, -(sig as i32));
                            crate::posix::process::schedule();
                            __halt_cpu()
                        } else {
                            // Inject signal frame
                            // We must copy mut_tf to the user stack, and change mut_tf.sepc to the handler.
                            // The user stack pointer is mut_tf.regs[2]
                            let mut sp = mut_tf.regs[2];
                            
                            // Align down to 16 bytes and allocate space for TrapFrame (272 bytes)
                            sp = (sp - core::mem::size_of::<TrapFrame>()) & !15;
                            
                            core::ptr::write(sp as *mut TrapFrame, *mut_tf);
                            
                            mut_tf.regs[2] = sp; // new sp
                            mut_tf.regs[10] = sig as usize; // arg0 = sig number
                            mut_tf.sepc = handler_addr; // jump to handler
                        }
                    }
                }
            }
        }
    }
    
    ret_tf
}
