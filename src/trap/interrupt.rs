use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;
use alloc::collections::BTreeMap;
use crate::sync::Mutex;

pub static IRQ_HANDLERS: Mutex<BTreeMap<u32, i32>> = Mutex::new(BTreeMap::new());

/// Drain all pending UART bytes into the active TTY's line discipline buffer
/// and wake any blocked stdin reader.  Called from the timer ISR (HART 0 only).
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
                // Send SIGINT (2) to the foreground process group.
                let table = crate::posix::process::PROCESS_TABLE.lock();
                for (_pid, proc) in table.iter() {
                    if proc.pgid.load(Ordering::Relaxed) == fg {
                        proc.pending_signals.fetch_or(1 << 2, Ordering::Relaxed);
                    }
                }
            } else {
                // No external fg process — set ctrlc on the active TTY.
                tty.ctrlc.store(true, Ordering::Relaxed);
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

/// Called from schedule()'s idle loop so UART chars are serviced when the
/// timer ISR cannot fire (SIE=0 in supervisor mode).
pub fn service_uart() {
    if crate::smp::current_hartid() == 0 {
        poll_uart_into_linedisc();
    }
}

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
