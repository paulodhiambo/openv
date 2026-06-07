//! # UART (Universal Asynchronous Receiver-Transmitter) Driver
//!
//! This module provides a driver for UART (serial port) devices, which are
//! the primary console output device for OpenV. The UART is used for all
//! kernel logging and debugging output.
//!
//! ## Hardware
//!
//! OpenV uses the NS16550-compatible UART, which is a common serial port
//! controller found in many RISC-V platforms. The NS16550 provides:
//!
//! - **Programmable baud rate**: Configurable via divisor registers
//! - **Data bits, parity, and stop bits**: Configurable via line control
//! - **FIFO buffering**: 16-byte FIFO for receive and transmit
//! - **Interrupt generation**: For receive data, transmit empty, and
//!   various error conditions
//!
//! ## Configuration
//!
//! The UART is configured for:
//! - 115200 baud (at 1.8432 MHz clock)
//! - 8 data bits
//! - No parity
//! - 1 stop bit (8N1)
//! - FIFO enabled with 14-byte threshold
//! - RTS, DTR, and OUT2 signals enabled
//! - Receive interrupt enabled
//!
//! ## Base Address
//!
//! The UART base address is platform-specific. The default is the QEMU
//! 'virt' platform address (`0x1000_0000`), but it can be overridden via
//! [`set_base`] or [`init_from_dtb`].
//!
//! ## Safety
//!
//! This driver uses volatile memory operations to interact with UART
//! registers. This is necessary because the UART is a memory-mapped
//! device and the compiler must not optimize away or reorder accesses
//! to its registers.

use core::sync::atomic::{AtomicUsize, Ordering};

// QEMU virt default; overridden at boot via `init_from_dtb` or `set_base`.
const UART_BASE_DEFAULT: usize = 0x1000_0000;

/// Atomic storage for the UART base address.
///
/// This is initialized to the QEMU 'virt' platform default and can be
/// overridden via [`set_base`] or [`init_from_dtb`]. It is stored as
/// an [`AtomicUsize`] to allow safe access from any context.
static UART_BASE: AtomicUsize = AtomicUsize::new(UART_BASE_DEFAULT);

/// Returns the current UART base address.
///
/// # Returns
///
/// The physical base address of the UART registers.
#[inline(always)]
fn base() -> usize {
    UART_BASE.load(Ordering::Relaxed)
}

/// Overrides the UART base address. Must be called before the first print.
///
/// # Arguments
///
/// * `addr` - The physical base address of the UART registers.
///
/// # Safety
///
/// The caller must ensure that `addr` is a valid UART base address and
/// that no concurrent accesses to the UART are occurring.
pub fn set_base(addr: usize) {
    UART_BASE.store(addr, Ordering::Relaxed);
}

/// Walks the DTB to find the stdout-path serial node and read its `reg` base
/// address. Falls back silently to the compiled-in default if anything is
/// absent or unparseable.
///
/// # Arguments
///
/// * `dtb_ptr` - The physical address of the device tree blob (DTB).
///
/// # Safety
///
/// This function should be called as early as possible in `kmain`, before
/// the first `println!`. The caller must ensure that `dtb_ptr` is a valid
/// DTB address.
///
/// # Implementation
///
/// This function:
///
/// 1. Parses the DTB to find the `/chosen` node.
/// 2. Follows the `stdout-path` property (including alias resolution)
///    to find the console device node.
/// 3. Reads the `reg` property to get the base address.
/// 4. Calls [`set_base`] to update the UART base address.
///
/// If any step fails, the function returns silently and the default
/// UART base address is used.
pub fn init_from_dtb(dtb_ptr: usize) {
    if dtb_ptr == 0 {
        return;
    }
    // SAFETY: dtb_ptr is the physical address of the DTB passed by OpenSBI in
    // register a1.  It is valid for the kernel lifetime and correctly aligned.
    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => return,
    };

    // chosen().stdout() follows /chosen/stdout-path (including alias resolution)
    // and returns the FdtNode for the console device.
    let node = match fdt.chosen().stdout() {
        Some(n) => n,
        None => return,
    };

    // `reg` on a serial node is (base_address, size).  On most boards the
    // parent bus `ranges` makes this a direct CPU physical address.
    if let Some(reg) = node.property("reg")
        && let Some(addr) = reg.as_usize()
        && addr != 0
    {
        set_base(addr);
    }
}

/// A zero-sized type representing a UART device.
///
/// This type is used to group UART-related methods together. It is
/// zero-sized, so creating a `Uart` instance has no runtime cost.
pub struct Uart;

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart {
    /// Creates a new `Uart` instance.
    ///
    /// # Returns
    ///
    /// A new `Uart` instance. This is a zero-sized type, so the
    /// instance carries no data.
    pub const fn new() -> Self {
        Uart
    }

    /// Programs NS16550 line-control and FIFO registers.
    ///
    /// This function configures the UART for:
    /// - 115200 baud (at 1.8432 MHz clock)
    /// - 8 data bits, no parity, 1 stop bit (8N1)
    /// - FIFO enabled with 14-byte threshold
    /// - RTS, DTR, and OUT2 signals enabled
    /// - Receive interrupt enabled
    ///
    /// # Safety
    ///
    /// This function performs volatile memory writes to UART registers.
    /// The caller must ensure that the UART base address is valid.
    pub fn init(&mut self) {
        let ptr = base() as *mut u8;
        unsafe {
            // Disable all interrupts
            ptr.add(1).write_volatile(0x00);
            // Set DLAB=1 to access the baud rate divisor registers
            ptr.add(3).write_volatile(0x80);
            // Divisor low byte (115200 baud @ 1.8432 MHz)
            ptr.add(0).write_volatile(0x03);
            // Divisor high byte
            ptr.add(1).write_volatile(0x00);
            // 8N1, DLAB=0
            ptr.add(3).write_volatile(0x03);
            // Enable and clear FIFO, 14-byte threshold
            ptr.add(2).write_volatile(0xC7);
            // RTS+DTR+OUT2
            ptr.add(4).write_volatile(0x0B);
            // Enable RX interrupt
            ptr.add(1).write_volatile(0x01);
        }
    }

    /// Writes a single byte to the UART.
    ///
    /// This function blocks until the transmitter holding register is
    /// empty, then writes the byte.
    ///
    /// # Arguments
    ///
    /// * `c` - The byte to write.
    ///
    /// # Safety
    ///
    /// This function performs volatile memory reads and writes to UART
    /// registers. The caller must ensure that the UART base address is
    /// valid.
    pub fn put_char(&mut self, c: u8) {
        let ptr = base() as *mut u8;
        unsafe {
            // Wait for transmitter holding register to be empty (LSR bit 5).
            // LSR bit 5 is the Transmitter Holding Register Empty (THRE) bit.
            while (ptr.add(5).read_volatile() & (1 << 5)) == 0 {}
            ptr.add(0).write_volatile(c);
        }
    }

    /// Attempts to read a byte from the UART without blocking.
    ///
    /// # Returns
    ///
    /// `Some(byte)` if a byte is available, `None` otherwise.
    ///
    /// # Safety
    ///
    /// This function performs volatile memory reads from UART registers.
    /// The caller must ensure that the UART base address is valid.
    pub fn try_get_char(&mut self) -> Option<u8> {
        let ptr = base() as *mut u8;
        unsafe {
            // Check LSR bit 0 (Data Ready) to see if a byte is available.
            if (ptr.add(5).read_volatile() & 1) != 0 {
                Some(ptr.add(0).read_volatile())
            } else {
                None
            }
        }
    }
}

/// Writes a single byte to the UART.
///
/// This is a convenience function that creates a temporary `Uart` instance
/// and calls [`Uart::put_char`].
///
/// # Arguments
///
/// * `c` - The byte to write.
pub fn write_char(c: u8) {
    Uart.put_char(c);
}

/// Writes a string slice to the UART.
///
/// This is a convenience function that creates a temporary `Uart` instance
/// and writes each byte of the string.
///
/// # Arguments
///
/// * `s` - The string slice to write.
pub fn write_str(s: &str) {
    for &b in s.as_bytes() {
        Uart.put_char(b);
    }
}

/// Writes a decimal representation of `v` to the UART.
///
/// # Arguments
///
/// * `v` - The value to write.
pub fn write_dec(mut v: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    write_str(unsafe { core::str::from_utf8_unchecked(&buf[i..]) });
}

/// Writes a hexadecimal representation of `v` (lower-case, no prefix) to the UART.
///
/// # Arguments
///
/// * `v` - The value to write.
pub fn write_hex(mut v: usize) {
    let mut buf = [0u8; 16];
    let mut i = buf.len();
    let hex = b"0123456789abcdef";
    loop {
        i -= 1;
        buf[i] = hex[v & 0xf];
        v >>= 4;
        if v == 0 {
            break;
        }
    }
    write_str(unsafe { core::str::from_utf8_unchecked(&buf[i..]) });
}

impl core::fmt::Write for Uart {
    /// Writes a string slice to the UART, converting `\n` to `\r\n`.
    ///
    /// This is required because many terminals expect a carriage return
    /// before a line feed.
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.put_char(b'\r');
            }
            self.put_char(b);
        }
        Ok(())
    }
}

/// Implementation of the `print!` macro.
///
/// This function is used by the `print!` macro to write formatted output
/// to the UART.
///
/// # Arguments
///
/// * `args` - The formatted arguments to write.
pub fn print_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let _ = Uart::new().write_fmt(args);
}

/// Implementation of the `println!` macro.
///
/// This function is used by the `println!` macro to write formatted output
/// to the UART, followed by a newline.
///
/// # Arguments
///
/// * `args` - The formatted arguments to write.
pub fn println_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let mut u = Uart::new();
    let _ = u.write_fmt(args);
    u.put_char(b'\r');
    u.put_char(b'\n');
}

/// Prints a string slice to the UART without any formatting.
///
/// This macro is useful for early debugging before the formatter is
/// fully initialized.
///
/// # Examples
///
/// ```ignore
/// raw_print!("Hello, world!\n");
/// ```
#[macro_export]
macro_rules! raw_print {
    ($s:expr) => {
        $crate::uart::write_str($s)
    };
}

/// Prints formatted output to the UART.
///
/// This macro is the primary way to write output to the console.
///
/// # Examples
///
/// ```ignore
/// print!("Hello, {}!\n", "world");
/// ```
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::uart::print_impl(format_args!($($arg)*))
    };
}

/// Prints formatted output to the UART, followed by a newline.
///
/// This macro is the primary way to write output to the console.
///
/// # Examples
///
/// ```ignore
/// println!("Hello, {}!", "world");
/// ```
#[macro_export]
macro_rules! println {
    () => { $crate::uart::write_str("\r\n") };
    ($($arg:tt)*) => {
        $crate::uart::println_impl(format_args!($($arg)*))
    };
}
