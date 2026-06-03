// 10 ms at 10 MHz CLINT clock
pub const TIMER_INTERVAL: u64 = 100_000;

pub fn set_next_timer() {
    let now = riscv::register::time::read() as u64;
    sbi_set_timer(now + TIMER_INTERVAL);
}

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
