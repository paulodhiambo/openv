use ::core::fmt;

const UART_BASE: usize = 0x1000_0000;

pub struct Uart;

impl Uart {
    pub fn new() -> Self {
        Uart
    }

    pub fn init(&mut self) {
        let ptr = UART_BASE as *mut u8;
        unsafe {
            // Disable interrupts
            ptr.add(1).write_volatile(0x00);
            
            // Enable DLAB (set baud rate divisor)
            ptr.add(3).write_volatile(0x80);
            
            // Set divisor to 3 (lo byte) 38400 baud
            ptr.add(0).write_volatile(0x03);
            ptr.add(1).write_volatile(0x00);
            
            // 8 bits, no parity, one stop bit
            ptr.add(3).write_volatile(0x03);
            
            // Enable FIFO, clear them, with 14-byte threshold
            ptr.add(2).write_volatile(0xC7);
            
            // Mark data terminal ready, signal request to send
            // and enable auxilliary output 2 (used as interrupt line for CPU)
            ptr.add(4).write_volatile(0x0B);
            
            // Enable interrupts
            ptr.add(1).write_volatile(0x01);
        }
    }

    pub fn put_char(&mut self, c: u8) {
        let ptr = UART_BASE as *mut u8;
        unsafe {
            // Wait until the transmitter is empty
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

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.put_char(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        use ::core::fmt::Write;
        let mut uart = $crate::uart::Uart::new();
        let _ = uart.write_fmt(format_args!($($arg)*));
    });
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($fmt:expr) => ($crate::print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::print!(concat!($fmt, "\n"), $($arg)*));
}
