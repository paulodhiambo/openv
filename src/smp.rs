use core::sync::atomic::{AtomicBool, Ordering};

unsafe extern "C" {
    fn __halt_cpu() -> !;
}

pub const MAX_HARTS: usize = 4;

#[unsafe(no_mangle)]
pub static SMP_GO_FLAG: AtomicBool = AtomicBool::new(false);

#[repr(C, align(16))]
pub struct HartStack(pub [u8; 16_384]);

#[unsafe(no_mangle)]
pub static mut SECONDARY_STACKS: [HartStack; MAX_HARTS] = [
    HartStack([0; 16_384]),
    HartStack([0; 16_384]),
    HartStack([0; 16_384]),
    HartStack([0; 16_384]),
];

pub fn start_secondaries() {
    SMP_GO_FLAG.store(true, Ordering::Release);
}

#[unsafe(no_mangle)]
pub extern "C" fn secondary_kmain(hartid: usize, _dtb_ptr: usize) -> ! {
    crate::println!("HART {} online", hartid);
    crate::trap::init_hart();
    crate::trap::enable_timer();
    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}

pub fn send_ipi_all_except_self() {
    let self_hart: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) self_hart, options(nomem, nostack));
    }
    let self_hart = self_hart.min(MAX_HARTS - 1);
    let mut mask: usize = 0;
    for i in 0..MAX_HARTS {
        if i != self_hart {
            mask |= 1 << i;
        }
    }
    if mask != 0 {
        sbi_send_ipi(mask, 0);
    }
}

fn sbi_send_ipi(hart_mask: usize, hart_mask_base: usize) {
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") hart_mask => _,
            inlateout("a1") hart_mask_base => _,
            in("a6") 0usize,
            in("a7") 0x735049usize, // "sPI" IPI extension
            options(nostack),
        );
    }
}
