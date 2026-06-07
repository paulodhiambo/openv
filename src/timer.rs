//! # Timer Driver and Timekeeping
//!
//! This module provides a simple timer driver for OpenV, using the RISC-V
//! SBI (Supervisor Binary Interface) timer extension. The timer is used
//! for preemptive scheduling, allowing the kernel to interrupt user-space
//! processes at regular intervals.
//!
//! ## Timer Configuration
//!
//! The timer is configured to fire every [`TIMER_INTERVAL`] clock cycles.
//! On a 10 MHz CLINT clock, this corresponds to a 10 ms interval. The
//! actual interval can be adjusted by changing [`TIMER_INTERVAL`].
//!
//! ## SBI Timer Extension
//!
//! OpenV uses the legacy SBI timer extension (EID `0x54494D45`, which is
//! "TIME" in ASCII) to set the next timer interrupt. This is a simple
//! extension that takes a single argument: the absolute time at which
//! the timer should fire.
//!
//! ## Usage
//!
//! The timer is typically set up during kernel initialization and then
//! re-armed on each timer interrupt. The trap handler calls
//! [`set_next_timer`] to schedule the next interrupt.
//!
//! [`TIMER_INTERVAL`]: constant.TIMER_INTERVAL.html

/// Timer interval in clock cycles.
///
/// This is set to 100,000 cycles, which corresponds to 10 ms at a 10 MHz
/// CLINT clock. Adjust this value to change the timer interval.
pub const TIMER_INTERVAL: u64 = 100_000;

/// Sets the next timer interrupt to fire after [`TIMER_INTERVAL`] cycles.
///
/// This function reads the current time from the RISC-V `time` register
/// and schedules the next timer interrupt to fire at `now + TIMER_INTERVAL`.
///
/// # Implementation
///
/// Uses the SBI timer extension (EID `0x54494D45`, "TIME") to set the
/// timer. The SBI call takes a single argument: the absolute time at
/// which the timer should fire.
///
/// [`TIMER_INTERVAL`]: constant.TIMER_INTERVAL.html
pub fn set_next_timer() {
    let now = riscv::register::time::read() as u64;
    sbi_set_timer(now + TIMER_INTERVAL);
}

/// Makes an SBI call to the timer extension to set the next timer interrupt.
///
/// This function makes a supervisor binary interface (SBI) call to the
/// legacy timer extension (EID `0x54494D45`, "TIME"). The SBI call
/// takes a single argument: the absolute time at which the timer should fire.
///
/// # Arguments
///
/// * `stime_value` - The absolute time at which the timer should fire,
///   in clock cycles since system start.
///
/// # Safety
///
/// This function makes a privileged SBI call. The caller must ensure
/// that the SBI is available and that `stime_value` is a valid time value.
fn sbi_set_timer(stime_value: u64) {
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") stime_value as usize => _,
            in("a6") 0usize,
            in("a7") 0x54494D45usize, // "TIME" extension
            options(nostack),
        );
    }
}
