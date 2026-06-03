const UART_BASE: usize = 0x1000_0000;

pub struct Uart;

impl Uart {
    pub const fn new() -> Self {
        Uart
    }

    pub fn init(&mut self) {
        let ptr = UART_BASE as *mut u8;
        unsafe {
            ptr.add(1).write_volatile(0x00);
            ptr.add(3).write_volatile(0x80);
            ptr.add(0).write_volatile(0x03);
            ptr.add(1).write_volatile(0x00);
            ptr.add(3).write_volatile(0x03);
            ptr.add(2).write_volatile(0xC7);
            ptr.add(4).write_volatile(0x0B);
            ptr.add(1).write_volatile(0x01);
        }
    }

    pub fn put_char(&mut self, c: u8) {
        let ptr = UART_BASE as *mut u8;
        unsafe {
            while (ptr.add(5).read_volatile() & (1 << 5)) == 0 {}
            ptr.add(0).write_volatile(c);
        }
    }

    pub fn try_get_char(&mut self) -> Option<u8> {
        let ptr = UART_BASE as *mut u8;
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
    let mut u = Uart;
    u.put_char(c);
}

/// Write a string slice to the UART.
pub fn write_str(s: &str) {
    let mut u = Uart;
    for &b in s.as_bytes() {
        u.put_char(b);
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

/// Backing function for `print!` — avoids `use` imports inside macro bodies.
pub fn print_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let _ = Uart::new().write_fmt(args);
}

/// Backing function for `println!`.
pub fn println_impl(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let mut u = Uart::new();
    let _ = u.write_fmt(args);
    u.put_char(b'\r');
    u.put_char(b'\n');
}

/// Raw print a string literal (no formatting).
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
