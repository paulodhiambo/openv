//! # Symmetric Multiprocessing (SMP) Support
//!
//! This module provides support for symmetric multiprocessing (SMP) on
//! multi-hart (hardware thread) RISC-V systems. SMP allows multiple harts
//! to execute code simultaneously, improving system performance and
//! responsiveness.
//!
//! ## Overview
//!
//! OpenV's SMP support is designed to be simple and efficient. The primary
//! hart (usually HART 0) is responsible for initializing the kernel and
//! starting secondary harts. Once started, all harts execute the same
//! scheduler and can run user-space processes in parallel.
//!
//! ## Hart Stacks
//!
//! Each secondary hart has its own kernel stack, allocated from a static
//! array of [`HartStack`] structures. Each stack includes:
//!
//! - **Guard page** (4 KB): A guard region filled with [`KSTACK_CANARY`]
//!   that is checked by the scheduler to detect stack overflows.
//!  - **Stack** (16 KB): The actual kernel stack used by the hart.
//!
//! The guard page is placed at the low end of the structure, and the stack
//! grows downward from the top. If the stack overflows into the guard page,
//! the canary bytes will be overwritten, which the scheduler can detect.
//!
//! ## Inter-Processor Interrupts (IPIs)
//!
//! OpenV uses the SBI (Supervisor Binary Interface) IPI extension to send
//! inter-processor interrupts between harts. This is used for TLB shootdown
//! and other cross-hart notifications.
//!
//! ## Hart Identification
//!
//! The current hart ID is stored in the `tp` (thread pointer) register.
//! This is a RISC-V convention that allows fast access to per-hart data
//! without needing to read a global variable.
//!
//! ## Safety
//!
//! SMP support requires careful synchronization to prevent race conditions.
//! All shared data structures must be protected by locks or use atomic
//! operations. The `tp` register is used to store the hart ID and must
//! be set up correctly during boot.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Defined in main.rs global_asm; prevents the compiler from optimizing away the halt loop.
unsafe extern "C" {
    fn __halt_cpu() -> !;
}

/// HART ID of the primary boot HART (whichever won the atomic election in boot.s).
/// Used to restrict UART polling to one HART regardless of which hartid won the race.
pub static BOOT_HARTID: AtomicUsize = AtomicUsize::new(0);

/// Maximum number of harts (hardware threads) supported by the kernel.
///
/// This is set to 4, which is the maximum number of harts in the QEMU
/// 'virt' platform. If you are using a platform with more harts, you will
/// need to increase this value and adjust the secondary stack allocation.
pub const MAX_HARTS: usize = 4;

/// Global flag indicating that secondary harts should start executing.
///
/// This flag is set to `true` by [`start_secondaries`] to signal to the
/// secondary harts that they should begin executing. The secondary harts
/// wait in a loop until this flag is set.
///
/// The flag is stored as an [`AtomicBool`] to allow the primary hart to
/// signal the secondary harts without requiring a lock.
///
/// [`AtomicBool`]: https://doc.rust-lang.org/core/sync/atomic/struct.AtomicBool.html
#[unsafe(no_mangle)]
pub static SMP_GO_FLAG: AtomicBool = AtomicBool::new(false);

/// Per-secondary-HART stack with a 4 KB guard page at the bottom.
///
/// The guard region is filled with [`KSTACK_CANARY`] and checked by the
/// scheduler to detect stack overflows.
///
/// # Layout
///
/// The layout from low to high is:
/// - `[guard: 4096 B]`: Guard page filled with [`KSTACK_CANARY`].
/// - `[stack: 16384 B]`: The actual kernel stack.
///
/// The stack pointer is initialized to the top of `stack`, and the stack
/// grows downward. If the stack overflows into the guard page, the canary
/// bytes will be overwritten, which the scheduler can detect.
///
/// # Alignment
///
/// The struct is aligned to 4096 bytes (a page boundary) to ensure that
/// the guard page is properly aligned and can be used for memory protection
/// if needed.
///
/// [`KSTACK_CANARY`]: constant.KSTACK_CANARY.html
#[repr(C, align(4096))]
pub struct HartStack {
    /// Guard page filled with [`KSTACK_CANARY`]. If the stack overflows
    /// into this region, the canary bytes will be overwritten.
    pub _guard: [u8; 4096],
    /// The actual kernel stack used by the hart.
    pub stack:  [u8; 16_384],
}

/// Canary byte used to detect kernel stack overflows.
///
/// This value (`0xCD`) is written to the guard page of each [`HartStack`].
/// If the kernel stack overflows into the guard page, this byte will be
/// overwritten, which the scheduler can detect by checking the guard page.
pub const KSTACK_CANARY: u8 = 0xCD;

/// Static array of per-hart stacks for secondary harts.
///
/// This array contains one [`HartStack`] for each possible secondary hart.
/// The primary hart (HART 0) uses a separate stack allocated by the boot
/// assembly.
///
/// # Safety
///
/// This static is marked as `static mut` because it is modified during
/// initialization. However, it is only modified before secondary harts
/// are started, so there are no data races.
#[unsafe(no_mangle)]
pub static mut SECONDARY_STACKS: [HartStack; MAX_HARTS] = [
    HartStack { _guard: [KSTACK_CANARY; 4096], stack: [0; 16_384] },
    HartStack { _guard: [KSTACK_CANARY; 4096], stack: [0; 16_384] },
    HartStack { _guard: [KSTACK_CANARY; 4096], stack: [0; 16_384] },
    HartStack { _guard: [KSTACK_CANARY; 4096], stack: [0; 16_384] },
];

/// Signals to secondary harts that they should start executing.
///
/// This function sets [`SMP_GO_FLAG`] to `true`, which signals to the
/// secondary harts (which are waiting in a loop) that they should begin
/// executing. This should be called once during kernel boot, after all
/// kernel data structures have been initialized.
///
/// # Implementation
///
/// Uses [`Ordering::Release`] to ensure that all prior memory writes are
/// visible to the secondary harts before they read the flag.
///
/// [`SMP_GO_FLAG`]: static.SMP_GO_FLAG.html
/// [`Ordering::Release`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Release
pub fn start_secondaries() {
    SMP_GO_FLAG.store(true, Ordering::Release);
}

/// Entry point for secondary harts.
///
/// This function is called by the boot code on each secondary hart after
/// [`SMP_GO_FLAG`] is set. It initializes the trap handler, enables the
/// timer interrupt, and enters the scheduler.
///
/// # Arguments
///
/// * `hartid` - The hart (hardware thread) ID of the current CPU.
/// * `_dtb_ptr` - The device tree blob pointer (unused on secondary harts).
///
/// # Returns
///
/// This function never returns (`!`). After the scheduler returns (which
/// should never happen), the CPU is halted via [`__halt_cpu`].
///
/// # Safety
///
/// This function is called from assembly and must not be called directly.
/// It is marked as `#[unsafe(no_mangle)]` to ensure that the symbol is
/// not mangled by the Rust compiler.
///
/// [`SMP_GO_FLAG`]: static.SMP_GO_FLAG.html
/// [`__halt_cpu`]: ../fn.__halt_cpu.html
#[unsafe(no_mangle)]
#[allow(unreachable_code)]
pub extern "C" fn secondary_kmain(hartid: usize, _dtb_ptr: usize) -> ! {
    crate::println!("HART {} online", hartid);
    // Initialize the trap handler for this hart. This sets up the trap
    // vector and configures the CPU to handle traps.
    crate::trap::init_hart();
    // Enable the timer interrupt for preemption.
    crate::trap::enable_timer();
    // Enter the scheduler. This function never returns.
    crate::posix::process::schedule();
    // If the scheduler does return (which should never happen), halt the CPU.
    unsafe { __halt_cpu() }
}

/// Returns the current hart (hardware thread) ID.
///
/// The hart ID is stored in the `tp` (thread pointer) register, which is
/// a RISC-V convention for fast per-hart data access. The value is
/// clamped to `[0, MAX_HARTS - 1]` to prevent out-of-bounds access.
///
/// # Returns
///
/// The current hart ID, guaranteed to be in the range `[0, MAX_HARTS - 1]`.
///
/// # Safety
///
/// This function reads the `tp` register, which must be set up correctly
/// during boot. If `tp` contains an invalid value, the returned hart ID
/// may be incorrect.
pub fn current_hartid() -> usize {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp, options(nomem, nostack));
    }
    tp.min(MAX_HARTS - 1)
}

/// Sends an inter-processor interrupt (IPI) to all harts.
///
/// This function sends an IPI to all harts, which is used for TLB shootdown
/// and other cross-hart notifications. The IPI is delivered via the SBI
/// IPI extension.
///
/// # Implementation
///
/// Builds a bitmask of all harts and calls [`sbi_send_ipi`] with the mask.
/// The SBI call uses the legacy `sPI` extension (EID `0x735049`).
pub fn send_ipi_all() {
    let mut mask: usize = 0;
    for i in 0..MAX_HARTS {
        mask |= 1 << i;
    }
    if mask != 0 {
        sbi_send_ipi(mask, 0);
    }
}


/// Sends an inter-processor interrupt (IPI) via SBI.
///
/// This function makes an SBI call to the IPI extension to send an IPI
/// to the specified harts. The SBI call uses the legacy `sPI` extension
/// (EID `0x735049`).
///
/// # Arguments
///
/// * `hart_mask` - Bitmask of harts to interrupt. Bit `i` corresponds to
///   hart `i + hart_mask_base`.
/// * `hart_mask_base` - Base hart ID for the bitmask.
///
/// # Safety
///
/// This function makes a supervisor binary interface (SBI) call, which
/// is a privileged operation. The caller must ensure that the SBI is
/// available and that the hart mask is valid.
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
