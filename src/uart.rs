use core::sync::atomic::{AtomicUsize, Ordering};

// QEMU virt default; overridden at boot via `init_from_dtb` or `set_base`.
const UART_BASE_DEFAULT: usize = 0x1000_0000;

static UART_BASE: AtomicUsize = AtomicUsize::new(UART_BASE_DEFAULT);

#[inline(always)]
fn base() -> usize {
    UART_BASE.load(Ordering::Relaxed)
}

/// Override the UART base address.  Must be called before the first print.
pub fn set_base(addr: usize) {
    UART_BASE.store(addr, Ordering::Relaxed);
}

/// Walk the DTB to find the stdout-path serial node and read its `reg` base
/// address.  Falls back silently to the compiled-in default if anything is
/// absent or unparseable.
///
/// Call this as early as possible in `kmain`, before the first `println!`.
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
    if let Some(reg) = node.property("reg") {
        if let Some(addr) = reg.as_usize() {
            if addr != 0 {
                set_base(addr);
            }
        }
    }
}

pub struct Uart;

impl Uart {
    pub const fn new() -> Self {
        Uart
    }

    /// Program NS16550 line-control and FIFO registers.
    pub fn init(&mut self) {
        let ptr = base() as *mut u8;
        unsafe {
            ptr.add(1).write_volatile(0x00); // disable interrupts
            ptr.add(3).write_volatile(0x80); // DLAB=1 (set baud rate divisor)
            ptr.add(0).write_volatile(0x03); // divisor low  (115200 baud @ 1.8432 MHz)
            ptr.add(1).write_volatile(0x00); // divisor high
            ptr.add(3).write_volatile(0x03); // 8N1, DLAB=0
            ptr.add(2).write_volatile(0xC7); // enable+clear FIFO, 14-byte threshold
            ptr.add(4).write_volatile(0x0B); // RTS+DTR+OUT2
            ptr.add(1).write_volatile(0x01); // enable RX interrupt
        }
    }

    pub fn put_char(&mut self, c: u8) {
        let ptr = base() as *mut u8;
        unsafe {
            // Wait for transmitter holding register to be empty (LSR bit 5).
            while (ptr.add(5).read_volatile() & (1 << 5)) == 0 {}
            ptr.add(0).write_volatile(c);
        }
    }

    pub fn try_get_char(&mut self) -> Option<u8> {
        let ptr = base() as *mut u8;
        unsafe {
            if (ptr.add(5).read_volatile() & 1) != 0 {
                Some(ptr.add(0).read_volatile())
            } else {
                None
            }
        }
    }
}

/// Write a single byte to the UART.
pub fn write_char(c: u8) {
    Uart.put_char(c);
}

/// Write a string slice to the UART.
pub fn write_str(s: &str) {
    for &b in s.as_bytes() {
        Uart.put_char(b);
    }
}

/// Write a decimal representation of `v`.
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

/// Write a hexadecimal representation of `v` (lower-case, no prefix).
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

pub fn print_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let _ = Uart::new().write_fmt(args);
}

pub fn println_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let mut u = Uart::new();
    let _ = u.write_fmt(args);
    u.put_char(b'\r');
    u.put_char(b'\n');
}

#[macro_export]
macro_rules! raw_print {
    ($s:expr) => {
        $crate::uart::write_str($s)
    };
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::uart::print_impl(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::uart::write_str("\r\n") };
    ($($arg:tt)*) => {
        $crate::uart::println_impl(format_args!($($arg)*))
    };
}
