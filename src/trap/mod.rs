//! # Trap Handling
//!
//! This module provides trap (exception and interrupt) handling for
//! OpenV. Traps are the mechanism by which the CPU transfers control
//! to the kernel in response to synchronous exceptions (e.g., page
//! faults, ecall) or asynchronous interrupts (e.g., timer, external).
//!
//! ## Overview
//!
//! The trap handler is defined in assembly (`trap_vector`) and dispatches
//! to [`rust_trap_handler`], which handles:
//!
//! - **User ecall** (exception 8): System calls, dispatched via
//!    [`crate::syscall::dispatch`].
//!  - **Page faults** (exceptions 12, 13, 15): Handled via
//!    [`page_fault::handle_page_fault`].
//!  - **Interrupts** (1, 5, 9, 11): Handled via
//!    [`interrupt::handle_interrupt`].
//!  - **Other exceptions**: Misaligned access, illegal instruction, etc.
//!  - **Signal delivery**: Before returning to user-space, any pending
//!    signals are delivered via the user's signal handler.
//!
//! ## Trap Frame
//!
//! The [`TrapFrame`] structure holds the saved register state of a
//! process when it was trapped. The trap handler saves all 32 general-purpose
//! registers, `sepc`, and `sstatus`. The `kernel_sp` field is used to
//! switch to the kernel stack.
//!
//! ## Signal Delivery
//!
//! Before returning to user-space, the trap handler checks for pending
//! signals. If a signal is pending and not blocked, the handler:
//! 1. Saves the current trap frame on the user stack.
//! 2. Sets `sepc` to the signal handler address.
//! 3. Sets `regs[1]` (ra) to the signal restorer address.
//! 4. Updates the blocked signals mask.
//! 5. Returns to user-space, which will execute the signal handler.
//!
//! ## Server PIDs
//!
//! The module tracks the PIDs of the VFS server ([`VFS_SERVER_PID`])
//! and the block driver server ([`BLK_SERVER_PID`]). These are used
//! for capability-based IPC.

pub mod interrupt;
pub mod page_fault;

use crate::println;
use core::arch::global_asm;
use core::sync::atomic::Ordering;
use riscv::register::{scause, sepc, stvec};

// Defined in main.rs global_asm.
unsafe extern "C" {
    pub(crate) fn __halt_cpu() -> !;
}

/// Halts the CPU in an infinite loop.
///
/// # Safety
///
/// This function never returns (`!`).
pub unsafe fn halt_cpu() -> ! {
    unsafe { __halt_cpu() }
}

use core::sync::atomic::AtomicI32;

/// PID of the registered VFS server process (`0` = not yet started).
pub static VFS_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// PID of the registered block driver process (`0` = not yet started).
pub static BLK_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// PID of the registered process-manager server (`0` = kernel handles fork/exit/waitpid).
pub static PM_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// PID of the registered procfs server (`0` = VFS server handles /proc inline).
pub static PROC_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// PID of the registered devfs server (`0` = VFS server handles /dev inline).
pub static DEV_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// PID of the registered component manager (`0` = not yet started).
pub static CM_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// The trap frame structure, holding saved register state.
///
/// # Fields
///
/// * `kernel_sp` - The kernel stack pointer.
/// * `regs` - The 32 general-purpose registers (x0 to x31).
/// * `sepc` - The saved program counter.
/// * `sstatus` - The saved status register.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// The kernel stack pointer.
    pub kernel_sp: usize,
    /// The 32 general-purpose registers (x0 to x31).
    pub regs: [usize; 32],
    /// The saved program counter.
    pub sepc: usize,
    /// The saved status register.
    pub sstatus: usize,
    /// The HART id of the HART that last dispatched this process.
    /// Stored here so the trap-entry path can restore tp = hartid even
    /// when arriving from U-mode (where tp holds a user-space value).
    pub kernel_hartid: usize,
}

/// Signal frame pushed to the user stack on signal delivery.
///
/// `sys_sigreturn` reads this back to restore both the trap frame and
/// the signal mask.
///
/// # Fields
///
/// * `saved_blocked` - The blocked signals mask at the time of delivery.
/// * `_pad` - Padding for alignment.
/// * `tf` - The saved trap frame.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SignalFrame {
    /// The blocked signals mask at the time of delivery.
    pub saved_blocked: u32,
    /// Padding for alignment.
    pub _pad: u32,
    /// The saved trap frame.
    pub tf: TrapFrame,
}

impl Default for TrapFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl TrapFrame {
    /// Creates a new zeroed trap frame.
    pub const fn new() -> Self {
        TrapFrame {
            kernel_sp: 0,
            regs: [0; 32],
            sepc: 0,
            sstatus: 0,
            kernel_hartid: 0,
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
    ld tp, 34*8+8(a0)    # U-mode: restore kernel tp = hartid
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
    /// Returns to user-space using the given trap frame.
    ///
    /// # Safety
    ///
    /// The trap frame must be valid and the address space must be set
    /// up correctly. This function never returns.
    pub fn return_to_user(trap_frame: usize) -> !;
}

/// Initializes the trap handler.
///
/// This function:
/// 1. Sets the trap vector to [`trap_vector`].
/// 2. Enables supervisor software interrupts (for TLB shootdown).
/// 3. Enables supervisor external interrupts.
///
/// Timer interrupts are NOT enabled here — they are enabled right
/// before the first call to `schedule()` to avoid firing while
/// `sscratch` is still 0.
pub fn init() {
    unsafe {
        stvec::write(trap_vector as *const () as usize, stvec::TrapMode::Direct);
        // Enable supervisor software interrupts (for TLB shootdown).
        // Timer interrupts are NOT enabled here — they are enabled right
        // before the first call to schedule() to avoid firing while
        // sscratch is still 0 (which would corrupt address zero in the
        // trap vector's csrrw sp, sscratch, sp).
        riscv::register::sie::set_ssoft();
        riscv::register::sie::set_sext();
    }
    println!("Trap handler initialized.");
}

/// Called by secondary HARTs from `secondary_kmain` — same as [`init`] without the print.
pub fn init_hart() {
    unsafe {
        stvec::write(trap_vector as *const () as usize, stvec::TrapMode::Direct);
        riscv::register::sie::set_ssoft();
        riscv::register::sie::set_sext();
    }
}

/// Enables timer interrupts and arms the first timer.
///
/// Must be called right before the first `schedule()`, after `sscratch`
/// has been initialised by a prior `return_to_user` or kernel-mode
/// scratch page.
pub fn enable_timer() {
    unsafe {
        riscv::register::sie::set_stimer();
    }
    crate::timer::set_next_timer();
}

/// The main trap handler, called from the assembly [`trap_vector`].
///
/// This function dispatches the trap to the appropriate handler based
/// on the `scause` register. After handling, it checks for pending
/// signals and delivers them if necessary.
///
/// # Arguments
///
/// * `tf` - A mutable reference to the trap frame.
///
/// # Returns
///
/// A pointer to the (possibly modified) trap frame. The assembly code
/// uses this to restore the register state.
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
        scause::Trap::Exception(12) | scause::Trap::Exception(13) | scause::Trap::Exception(15) => {
            let num = match cause {
                scause::Trap::Exception(x) => x,
                _ => unreachable!(),
            };
            page_fault::handle_page_fault(num, sepc, stval, tf)
        }
        scause::Trap::Interrupt(1) | scause::Trap::Interrupt(5) | scause::Trap::Interrupt(9) | scause::Trap::Interrupt(11) => {
            let num = match cause {
                scause::Trap::Interrupt(x) => x,
                _ => unreachable!(),
            };
            interrupt::handle_interrupt(num, tf)
        }
        scause::Trap::Interrupt(n) => {
            interrupt::handle_interrupt(n, tf)
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
            unsafe { halt_cpu() }
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
            unsafe { halt_cpu() }
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
            unsafe { halt_cpu() }
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
            unsafe { halt_cpu() }
        }
    };

    // If we are returning to user-space (SPP == 0), check for pending signals
    unsafe {
        let mut_tf = &mut *ret_tf;
        if (mut_tf.sstatus & (1 << 8)) == 0 {
            let pid = crate::posix::process::current_pid();
            let mut root_pa = 0;
            let mut new_sp = 0;
            let mut new_sp_end = 0;
            let mut sig = 0u32;
            let mut handler_addr = 0;
            let mut restorer_addr = 0;
            let extra_mask;
            let mut new_blocked = 0;
            let mut saved_blocked = 0;
            let mut do_deliver = false;
            let frame_size = core::mem::size_of::<SignalFrame>();

            // Phase 1: extract all needed data from PROCESS_TABLE under the lock.
            // The lock is dropped before any VMM or user-memory access to avoid
            // deadlock when handle_user_page_fault re-enters PROCESS_TABLE.
            {
                let table = crate::posix::process::PROCESS_TABLE.lock();
                if let Some(proc) = table.get(&pid) {
                    let pending = proc.pending_signals.load(core::sync::atomic::Ordering::Relaxed);
                    let blocked = proc.blocked_signals.load(core::sync::atomic::Ordering::Relaxed);
                    let deliverable = pending & !blocked;
                    if deliverable != 0 {
                        sig = deliverable.trailing_zeros();
                        proc.pending_signals.fetch_and(!(1 << sig), core::sync::atomic::Ordering::Relaxed);

                        // SIGKILL — unconditional exit
                        if sig == crate::syscall::proc::SIGKILL as u32 {
                            drop(table);
                            crate::posix::spawn::exit(pid, -9);
                            crate::posix::process::schedule();
                            halt_cpu()
                        }

                        handler_addr = proc.signal_handlers.lock()[sig as usize];
                        restorer_addr = proc.signal_restorers.lock()[sig as usize];

                        // Determine action: continue, stop, ignore, terminate, or deliver
                        let sig_is_stop = sig == crate::syscall::proc::SIGSTOP as u32
                            || sig == crate::syscall::proc::SIGTSTP as u32
                            || sig == crate::syscall::proc::SIGTTIN as u32
                            || sig == crate::syscall::proc::SIGTTOU as u32;

                        if sig == crate::syscall::proc::SIGCONT as u32 {
                            // SIGCONT: resume if stopped, clear pending stop signals
                            let stop_mask = (1 << crate::syscall::proc::SIGSTOP)
                                | (1 << crate::syscall::proc::SIGTSTP)
                                | (1 << crate::syscall::proc::SIGTTIN)
                                | (1 << crate::syscall::proc::SIGTTOU);
                            proc.pending_signals.fetch_and(!(stop_mask as u32), core::sync::atomic::Ordering::Relaxed);
                            let mut st = proc.state.lock();
                            if matches!(*st, crate::posix::process::ProcState::Stopped) {
                                *st = crate::posix::process::ProcState::Running;
                                crate::posix::process::enqueue_with_prio(
                                    pid,
                                    proc.priority.load(core::sync::atomic::Ordering::Relaxed),
                                );
                            }
                            drop(st);
                            drop(table);
                        } else if sig_is_stop && handler_addr == 0 {
                            // Stop signal with SIG_DFL: stop the process
                            let mut st = proc.state.lock();
                            *st = crate::posix::process::ProcState::Stopped;
                            drop(st);
                            drop(table);
                            crate::posix::process::schedule();
                            halt_cpu()
                        } else if handler_addr == 1
                            || (handler_addr == 0 && sig == crate::syscall::proc::SIGCHLD as u32)
                        {
                            // SIG_IGN or ignore-by-default: discard
                            drop(table);
                        } else if handler_addr == 0 {
                            // SIG_DFL for all other signals: terminate
                            drop(table);
                            crate::posix::spawn::exit(pid, -(sig as i32));
                            crate::posix::process::schedule();
                            halt_cpu()
                        } else {
                            // Deliver to user handler
                            let sp = mut_tf.regs[2];
                            if sp < frame_size + 16 || sp > 0x0000_8000_0000_0000 {
                                drop(table);
                                crate::posix::spawn::exit(pid, -11); // SIGSEGV
                                crate::posix::process::schedule();
                                halt_cpu()
                            }

                            new_sp = (sp - frame_size) & !15;
                            new_sp_end = new_sp + frame_size - 1;
                            saved_blocked = blocked;
                            extra_mask = proc.signal_masks.lock()[sig as usize];
                            new_blocked = blocked | extra_mask | (1 << sig);
                            let satp = proc.satp_val.load(Ordering::Relaxed);
                            root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
                            do_deliver = true;
                        }
                    }
                }
            } // PROCESS_TABLE unlocked here

            if do_deliver {
                // Phase 2: VMM operations -- PROCESS_TABLE is NOT held, safe to
                // call handle_user_page_fault which may re-enter PROCESS_TABLE.
                if crate::mm::vmm::PageTable::walk_page_table(root_pa, new_sp).is_err() {
                    if crate::mm::vmm::handle_user_page_fault(root_pa, new_sp, 0).is_err() {
                        crate::posix::spawn::exit(pid, -11);
                        crate::posix::process::schedule();
                        halt_cpu()
                    }
                }
                if (new_sp / 4096) != (new_sp_end / 4096)
                    && crate::mm::vmm::PageTable::walk_page_table(root_pa, new_sp_end).is_err()
                {
                    if crate::mm::vmm::handle_user_page_fault(root_pa, new_sp_end, 0).is_err() {
                        crate::posix::spawn::exit(pid, -11);
                        crate::posix::process::schedule();
                        halt_cpu()
                    }
                }

                let frame = SignalFrame {
                    saved_blocked,
                    _pad: 0,
                    tf: *mut_tf,
                };
                core::ptr::write(new_sp as *mut SignalFrame, frame);
                mut_tf.regs[2] = new_sp;
                mut_tf.regs[10] = sig as usize;
                mut_tf.sepc = handler_addr;
                mut_tf.regs[1] = restorer_addr;

                // Phase 3: re-acquire PROCESS_TABLE to update blocked mask.
                let table = crate::posix::process::PROCESS_TABLE.lock();
                if let Some(proc) = table.get(&pid) {
                    proc.blocked_signals.store(new_blocked, Ordering::Relaxed);
                }
            }
        }
    }
    
    ret_tf
}
