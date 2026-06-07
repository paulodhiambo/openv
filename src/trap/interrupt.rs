//! # Interrupt Handling
//!
//! This module handles RISC-V interrupts (asynchronous traps) on
//! behalf of the [`crate::trap`] module. RISC-V interrupt causes are:
//!
//! | Cause | Name                        |
//! |-------|-----------------------------|
//! | 1     | Supervisor software (IPI)   |
//! | 5     | Supervisor timer            |
//! | 9     | Supervisor external         |
//! | 11    | Machine external? (not used)|
//!
//! ## Timer Interrupt (Cause 5)
//!
//! The timer ISR:
//! 1. Arms the next timer via [`crate::timer::set_next_timer`].
/// 2. Wakes sleeping processes via [`crate::posix::process::wake_sleepers`].
/// 3. Polls the UART (HART 0 only) via [`poll_uart_into_linedisc`].
/// 4. Preempts the current process if there is another runnable process
///    in the run queue.
///
/// ## Software Interrupt (Cause 1)
///
/// Used for inter-processor TLB shootdown. On receive:
/// 1. Performs an `sfence.vma` to flush the local TLB.
/// 2. Clears the SSIP bit in `sip`.
///
/// ## External Interrupts (Cause 9)
///
/// External interrupts come from the PLIC. We claim the IRQ, dispatch
/// to the registered driver or user-space driver, then complete the
/// IRQ.

use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;
use alloc::collections::BTreeMap;
use crate::sync::Mutex;

/// Global table mapping IRQ number → owner PID.
///
/// When a user-space driver registers for an IRQ via the
/// `sys_irq_register` syscall, its PID is added to this table. On
/// external interrupt, the corresponding message is sent to the
/// registered process.
pub static IRQ_HANDLERS: Mutex<BTreeMap<u32, i32>> = Mutex::new(BTreeMap::new());

/// Drains all pending UART bytes into the active TTY's line discipline
/// buffer and wakes any blocked stdin reader. Called from the timer
/// ISR (HART 0 only).
///
/// # Line Discipline Behaviour
///
/// - `Ctrl-C` (0x03): Sends `SIGINT` (signal 2) to the foreground
///   process group. Prints `^C\n` to the UART. If no foreground
///   process, the byte is pushed into the TTY buffer as a printable
///   raw byte.
/// - `CR` / `LF`: Newline. Echoes `\n` and pushes `\n` to the buffer.
/// - `BS` (0x08) / `DEL` (0x7F): Backspace. Removes last byte from
///   the buffer and echoes `\b \b`.
/// - Other: Echoes the byte (if echo is on) and pushes to the buffer.
///
/// # Wakeup
///
/// After draining, if any byte was pushed and there is a waiter PID
/// in `tty.waiter`, the waiter is added to the run queue.
pub fn poll_uart_into_linedisc() {
    let tty = match crate::tty::ACTIVE_TTY.lock().clone() {
        Some(t) => t,
        None => return,
    };

    let mut uart = crate::uart::Uart::new();
    let mut any_pushed = false;
    loop {
        let c = match uart.try_get_char() {
            Some(c) => c,
            None => break,
        };
        if c == 0x03 {
            let fg = crate::posix::process::FOREGROUND_PID.load(Ordering::Relaxed);
            uart.put_char(b'^');
            uart.put_char(b'C');
            uart.put_char(b'\n');
            if fg > 0 {
                // Send SIGINT to the foreground process group and wake any
                // processes that are blocked in a blocking syscall so the
                // signal is checked on their next return to user mode.
                let table = crate::posix::process::PROCESS_TABLE.lock();
                for (pid, proc) in table.iter() {
                    if proc.pgid.load(Ordering::Relaxed) == fg {
                        proc.pending_signals.fetch_or(1 << 2, Ordering::Relaxed);
                        let mut st = proc.state.lock();
                        if matches!(*st, crate::posix::process::ProcState::Stopped) {
                            *st = crate::posix::process::ProcState::Running;
                            crate::posix::process::RUN_QUEUE.lock().push_back(*pid);
                        }
                    }
                }
                drop(table);
                // Also interrupt any TTY waiter (process blocked in sys_read).
                tty.ctrlc.store(true, Ordering::Relaxed);
                let waiter = tty.waiter.swap(0, Ordering::Relaxed);
                if waiter > 0 {
                    crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
                }
            } else {
                // No external fg process — push 0x03 into the TTY buf so the
                // shell receives it as a printable Ctrl-C byte in raw mode.
                let mut buf = tty.buf.lock();
                buf.push_back(0x03);
                drop(buf);
                let waiter = tty.waiter.swap(0, Ordering::Relaxed);
                if waiter > 0 {
                    crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
                }
            }
            break;
        }
        let mut buf = tty.buf.lock();
        let echo = tty.echo.load(Ordering::Relaxed);
        let raw = tty.raw.load(Ordering::Relaxed);
        let pushed = if raw {
            buf.push_back(c);
            true
        } else if c == b'\r' || c == b'\n' {
            if echo {
                uart.put_char(b'\n');
            }
            buf.push_back(b'\n');
            true
        } else if c == 0x08 || c == 0x7F {
            if buf.pop_back().is_some() && echo {
                uart.put_char(0x08);
                uart.put_char(b' ');
                uart.put_char(0x08);
            }
            false
        } else {
            if echo {
                uart.put_char(c);
            }
            buf.push_back(c);
            true
        };
        drop(buf);
        if pushed {
            any_pushed = true;
        }
    }
    if any_pushed {
        let waiter = tty.waiter.swap(0, Ordering::Relaxed);
        if waiter > 0 {
            crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
        }
    }
}

/// Called from `schedule()`'s idle loop so UART chars are serviced
/// when the timer ISR cannot fire (SIE=0 in supervisor mode).
pub fn service_uart() {
    if crate::smp::current_hartid() == 0 {
        poll_uart_into_linedisc();
    }
}

/// Handles an interrupt given its cause number.
///
/// # Arguments
///
/// * `cause` - The interrupt cause from `scause`.
/// * `tf` - The trap frame of the interrupted context.
///
/// # Returns
///
/// A pointer to the (possibly modified) trap frame.
pub fn handle_interrupt(cause: usize, tf: &mut TrapFrame) -> *mut TrapFrame {
    match cause {
        // Supervisor timer interrupt — preempt current process.
        5 => {
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
                    unsafe { crate::trap::halt_cpu() }
                } else {
                    tf as *mut _
                }
            } else {
                tf as *mut _
            }
        }
        // Supervisor software interrupt — TLB shootdown IPI
        1 => {
            unsafe {
                core::arch::asm!("sfence.vma");
                core::arch::asm!("csrci sip, 2"); // clear SSIP
            }
            tf as *mut _
        }
        // External interrupts — PLIC claim/complete
        9 | 11 => {
            let hart = crate::smp::current_hartid();
            let irq = crate::plic::claim(hart as usize);
            if irq != 0 {
                // If it's UART (10), dispatch locally for now to keep console working
                if irq == 10 {
                    crate::drivers::dispatch_interrupt(irq as usize);
                    crate::plic::complete(hart as usize, irq);
                } else {
                    let mut handled_in_userspace = false;
                    {
                        let handlers = IRQ_HANDLERS.lock();
                        if let Some(&pid) = handlers.get(&irq) {
                            // Mask the interrupt at the PLIC so it doesn't fire again immediately
                            crate::plic::set_enable(hart as usize, irq, false);
                            
                            // Create hardware interrupt IPC message
                            let mut d = [0u8; 56];
                            d[0] = irq as u8;
                            d[1] = (irq >> 8) as u8;
                            d[2] = (irq >> 16) as u8;
                            d[3] = (irq >> 24) as u8;
                            
                            let msg = crate::ipc::msg::Message {
                                source: -2, // HARDWARE_INTERRUPT identifier
                                type_: -2, // HARDWARE_INTERRUPT opcode
                                data: d,
                            };
                            
                            // Send message to driver
                            crate::syscall::ipc::kernel_send_msg(pid, msg);
                            
                            // Acknowledge the interrupt
                            crate::plic::complete(hart as usize, irq);
                            handled_in_userspace = true;
                        }
                    }
                    if !handled_in_userspace {
                        crate::drivers::dispatch_interrupt(irq as usize);
                        crate::plic::complete(hart as usize, irq);
                    }
                }
            }
            tf as *mut _
        }
        n => {
            crate::println!("Unhandled interrupt: {}", n);
            tf as *mut _
        }
    }
}
