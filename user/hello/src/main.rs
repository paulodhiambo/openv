#![no_std]
#![no_main]

use libos::{exit, write};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let msg = b"hello: Hello from exec'd program!\n";
    write(1, msg.as_ptr(), msg.len());
    exit(0);
}
