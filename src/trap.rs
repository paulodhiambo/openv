use riscv::register::{stvec, scause, sstatus, sepc, sscratch};
use core::arch::global_asm;
use crate::println;
use alloc::vec::Vec;
use spin::Mutex;

static LINE_DISC_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ECHO_ENABLED:    Mutex<bool>   = Mutex::new(true);
static RAW_MODE:        Mutex<bool>   = Mutex::new(false);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub kernel_sp: usize,
    pub regs: [usize; 32], // x0 to x31
    pub sepc: usize,
    pub sstatus: usize,
}

impl TrapFrame {
    pub const fn new() -> Self {
        TrapFrame {
            kernel_sp: 0,
            regs: [0; 32],
            sepc: 0,
            sstatus: 0,
        }
    }
}

global_asm!(
    r#"
    .section .text
    .global trap_vector
    .align 4
trap_vector:
    # Swap sp and sscratch. Now sp points to TrapFrame, sscratch has user sp.
    csrrw sp, sscratch, sp
    
    # Save all general-purpose registers
    sd ra, 1*8+8(sp)
    
    # Save user sp
    csrr t0, sscratch
    sd t0, 2*8+8(sp)

    sd gp, 3*8+8(sp)
    sd tp, 4*8+8(sp)
    sd t0, 5*8+8(sp)
    sd t1, 6*8+8(sp)
    sd t2, 7*8+8(sp)
    sd s0, 8*8+8(sp)
    sd s1, 9*8+8(sp)
    sd a0, 10*8+8(sp)
    sd a1, 11*8+8(sp)
    sd a2, 12*8+8(sp)
    sd a3, 13*8+8(sp)
    sd a4, 14*8+8(sp)
    sd a5, 15*8+8(sp)
    sd a6, 16*8+8(sp)
    sd a7, 17*8+8(sp)
    sd s2, 18*8+8(sp)
    sd s3, 19*8+8(sp)
    sd s4, 20*8+8(sp)
    sd s5, 21*8+8(sp)
    sd s6, 22*8+8(sp)
    sd s7, 23*8+8(sp)
    sd s8, 24*8+8(sp)
    sd s9, 25*8+8(sp)
    sd s10, 26*8+8(sp)
    sd s11, 27*8+8(sp)
    sd t3, 28*8+8(sp)
    sd t4, 29*8+8(sp)
    sd t5, 30*8+8(sp)
    sd t6, 31*8+8(sp)

    # Save sepc and sstatus
    csrr t0, sepc
    sd t0, 32*8+8(sp)
    csrr t1, sstatus
    sd t1, 33*8+8(sp)

    # Move TrapFrame ptr to a0 as arg
    mv a0, sp
    
    # Switch to kernel stack!
    ld sp, 0(a0)
    
    # Call the Rust handler
    call rust_trap_handler
    
    # rust_trap_handler returns the TrapFrame ptr in a0
    .global return_to_user
return_to_user:
    mv sp, a0

    # Restore sepc and sstatus
    ld t0, 32*8+8(sp)
    csrw sepc, t0
    ld t1, 33*8+8(sp)
    csrw sstatus, t1

    # Restore registers
    ld ra, 1*8+8(sp)
    ld gp, 3*8+8(sp)
    ld tp, 4*8+8(sp)
    ld t0, 5*8+8(sp)
    ld t1, 6*8+8(sp)
    ld t2, 7*8+8(sp)
    ld s0, 8*8+8(sp)
    ld s1, 9*8+8(sp)
    ld a0, 10*8+8(sp)
    ld a1, 11*8+8(sp)
    ld a2, 12*8+8(sp)
    ld a3, 13*8+8(sp)
    ld a4, 14*8+8(sp)
    ld a5, 15*8+8(sp)
    ld a6, 16*8+8(sp)
    ld a7, 17*8+8(sp)
    ld s2, 18*8+8(sp)
    ld s3, 19*8+8(sp)
    ld s4, 20*8+8(sp)
    ld s5, 21*8+8(sp)
    ld s6, 22*8+8(sp)
    ld s7, 23*8+8(sp)
    ld s8, 24*8+8(sp)
    ld s9, 25*8+8(sp)
    ld s10, 26*8+8(sp)
    ld s11, 27*8+8(sp)
    ld t3, 28*8+8(sp)
    ld t4, 29*8+8(sp)
    ld t5, 30*8+8(sp)
    ld t6, 31*8+8(sp)

    # Load user sp into sscratch, and TrapFrame address remains in sp
    ld t0, 2*8+8(sp)
    csrw sscratch, t0
    
    # Swap sp and sscratch back
    csrrw sp, sscratch, sp

    sret
    "#
);

unsafe extern "C" {
    fn trap_vector();
    pub fn return_to_user(trap_frame: usize) -> !;
}

pub fn init() {
    unsafe {
        stvec::write(trap_vector as usize, stvec::TrapMode::Direct);
    }
    println!("Trap handler initialized.");
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_trap_handler(tf: &mut TrapFrame) -> *mut TrapFrame {
    let cause = scause::read().cause();
    
    match cause {
        scause::Trap::Exception(8) => {
            // User ecall
            let syscall_num = tf.regs[17]; // a7
            let arg0 = tf.regs[10]; // a0
            let arg1 = tf.regs[11]; // a1
            let arg2 = tf.regs[12]; // a2
            
            match syscall_num {
                0 => { // sys_yield
                    tf.sepc += 4;
                    crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                    crate::posix::process::schedule();
                }
                1 => { // sys_exit
                    crate::println!("Process {} exited with code {}", crate::posix::process::current_pid(), arg0);
                    crate::posix::spawn::exit(crate::posix::process::current_pid(), arg0 as i32);
                    crate::posix::process::schedule();
                }
                2 => { // sys_write
                    // arg0 = fd, arg1 = buf ptr, arg2 = len
                    let buf = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2) };
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(arg0 as u32) {
                        match obj {
                            crate::ipc::handle::KernelObject::Console => {
                                if let Ok(s) = core::str::from_utf8(buf) {
                                    crate::print!("{}", s);
                                }
                                tf.regs[10] = arg2; // Return bytes written
                            }
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let msg = crate::ipc::channel::Message {
                                    bytes: alloc::vec::Vec::from(buf),
                                    handles: alloc::vec::Vec::new(),
                                };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = arg2,
                                    Err(_) => tf.regs[10] = usize::MAX, // -1
                                }
                            }
                            crate::ipc::handle::KernelObject::File(fd_desc) => {
                                let buf = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2) };
                                let mut offset = fd_desc.offset.lock();
                                match fd_desc.vnode.write_at(*offset, buf) {
                                    Ok(n) => { *offset += n; tf.regs[10] = n; }
                                    Err(_) => { tf.regs[10] = usize::MAX; }
                                }
                            }
                            _ => {
                                tf.regs[10] = usize::MAX; // EBADF
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // Error
                    }
                }
                3 => { // sys_pipe
                    // arg0 = ptr to [u32; 2]
                    let (ep1, ep2) = crate::ipc::channel::ChannelEndpoint::create_pair();
                    let h1 = {
                        let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                        proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep1))
                    };
                    let h2 = {
                        let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                        proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep2))
                    };
                    unsafe {
                        let arr = arg0 as *mut [u32; 2];
                        (*arr)[0] = h1;
                        (*arr)[1] = h2;
                    }
                    tf.regs[10] = 0; // return 0
                }
                5 => { // sys_read
                    // arg0 = fd, arg1 = buf ptr, arg2 = max_len
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(arg0 as u32) {
                        match obj {
                            crate::ipc::handle::KernelObject::Console => {
                                // Raw mode: return one byte immediately (no line discipline)
                                if *RAW_MODE.lock() {
                                    let mut uart = crate::uart::Uart::new();
                                    if let Some(c) = uart.try_get_char() {
                                        let user_buf = unsafe {
                                            core::slice::from_raw_parts_mut(arg1 as *mut u8, 1)
                                        };
                                        user_buf[0] = c;
                                        tf.regs[10] = 1;
                                    } else {
                                        drop(fds);
                                        tf.sepc -= 4;
                                        crate::posix::process::RUN_QUEUE.lock()
                                            .push_back(crate::posix::process::current_pid());
                                        crate::posix::process::schedule();
                                    }
                                } else {

                                let mut uart = crate::uart::Uart::new();
                                let mut buf_lock = LINE_DISC_BUFFER.lock();

                                // Check if we already have a full line in the buffer
                                if let Some(newline_idx) = buf_lock.iter().position(|&b| b == b'\n') {
                                    let len = core::cmp::min(arg2, newline_idx + 1);
                                    let user_buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                                    for i in 0..len {
                                        user_buf[i] = buf_lock[i];
                                    }
                                    // Remove the consumed bytes
                                    buf_lock.drain(0..len);
                                    tf.regs[10] = len;
                                } else {
                                    // Poll for a character
                                    if let Some(c) = uart.try_get_char() {
                                        if c == b'\r' || c == b'\n' {
                                            uart.put_char(b'\n');
                                            buf_lock.push(b'\n');
                                            
                                            // Since we just got a newline, fulfill the read immediately
                                            let len = core::cmp::min(arg2, buf_lock.len());
                                            let user_buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                                            for i in 0..len {
                                                user_buf[i] = buf_lock[i];
                                            }
                                            buf_lock.drain(0..len);
                                            tf.regs[10] = len;
                                        } else if c == 0x08 || c == 0x7F { // Backspace or DEL
                                            if buf_lock.pop().is_some() {
                                                if *ECHO_ENABLED.lock() {
                                                    uart.put_char(0x08);
                                                    uart.put_char(b' ');
                                                    uart.put_char(0x08);
                                                }
                                            }
                                            // Restart syscall
                                            drop(buf_lock);
                                            drop(fds);
                                            tf.sepc -= 4;
                                        } else if c == 0x03 { // Ctrl-C
                                            uart.put_char(b'^');
                                            uart.put_char(b'C');
                                            uart.put_char(b'\n');
                                            buf_lock.clear();
                                            // Restart syscall
                                            drop(buf_lock);
                                            drop(fds);
                                            tf.sepc -= 4;
                                        } else {
                                            // Regular character
                                            buf_lock.push(c);
                                            if *ECHO_ENABLED.lock() {
                                                uart.put_char(c);
                                            }
                                            // Restart syscall to keep reading
                                            drop(buf_lock);
                                            drop(fds);
                                            tf.sepc -= 4;
                                        }
                                    } else {
                                        // Block by restarting the syscall and yielding
                                        drop(buf_lock);
                                        drop(fds);
                                        tf.sepc -= 4;
                                        crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                                        crate::posix::process::schedule();
                                    }
                                }
                                } // end cooked-mode else
                            }
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                if let Some(msg) = ep.try_recv() {
                                    let len = core::cmp::min(arg2, msg.bytes.len());
                                    let buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                                    buf.copy_from_slice(&msg.bytes[..len]);
                                    tf.regs[10] = len;
                                } else {
                                    // Block by restarting the syscall
                                    drop(fds);
                                    tf.sepc -= 4; // Rewind to ecall
                                    crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                                    crate::posix::process::schedule();
                                }
                            }
                            crate::ipc::handle::KernelObject::File(fd_desc) => {
                                let mut offset = fd_desc.offset.lock();
                                let buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, arg2) };
                                match fd_desc.vnode.read(*offset, buf) {
                                    Ok(bytes_read) => {
                                        *offset += bytes_read as usize;
                                        tf.regs[10] = bytes_read;
                                    }
                                    Err(_) => {
                                        tf.regs[10] = usize::MAX;
                                    }
                                }
                            }
                            _ => {
                                tf.regs[10] = usize::MAX; // EBADF
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // Error
                    }
                }
                6 => { // sys_spawn
                    crate::println!("sys_spawn: requested path at {:#x}, len {}", arg0, arg1);
                    // arg0 = path_ptr, arg1 = path_len
                    let path_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        crate::println!("sys_spawn: path string is '{}'", path);
                        if let Ok(child_pid) = crate::posix::spawn::posix_spawn(path, crate::posix::process::current_pid()) {
                            crate::println!("sys_spawn: successfully spawned pid {}", child_pid);
                            tf.regs[10] = child_pid as usize;
                        } else {
                            crate::println!("sys_spawn: posix_spawn failed");
                            tf.regs[10] = usize::MAX; // Error
                        }
                    } else {
                        crate::println!("sys_spawn: utf8 parse failed");
                        tf.regs[10] = usize::MAX; // Error
                    }
                }
                8 => { // sys_open
                    // arg0 = path_ptr, arg1 = path_len, arg2 = flags
                    let path_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        if let Ok(vnode) = crate::vfs::lookup_path(path) {
                            let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                            
                            // Basic flag decoding: 0=O_RDONLY, 1=O_WRONLY, 2=O_RDWR
                            let mut requested = 0;
                            if (arg2 & 3) == 0 || (arg2 & 3) == 2 {
                                requested |= crate::vfs::ACCESS_READ;
                            }
                            if (arg2 & 3) == 1 || (arg2 & 3) == 2 {
                                requested |= crate::vfs::ACCESS_WRITE;
                            }
                            
                            if crate::vfs::check_access(&proc, &*vnode, requested) {
                                let fd_desc = crate::ipc::handle::FileDescription {
                                    vnode,
                                    offset: spin::Mutex::new(0),
                                };
                                let fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::File(alloc::sync::Arc::new(fd_desc)));
                                tf.regs[10] = fd as usize;
                            } else {
                                crate::println!("sys_open: access denied to '{}'", path);
                                tf.regs[10] = usize::MAX - 12; // EACCES
                            }
                        } else {
                            tf.regs[10] = usize::MAX; // ENOENT
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // EINVAL
                    }
                }
                9 => { // sys_close
                    // arg0 = fd
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    if proc.fds.lock().remove(arg0 as u32).is_some() {
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX; // EBADF
                    }
                }
                12 => { // sys_getdents — arg0=path_ptr, arg1=path_len, arg2=buf_ptr, a3=buf_len
                    let buf_len = tf.regs[13]; // a3
                    let path_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        if let Ok(vnode) = crate::vfs::lookup_path(path) {
                            match vnode.readdir() {
                                Ok(entries) => {
                                    let buf = unsafe {
                                        core::slice::from_raw_parts_mut(arg2 as *mut u8, buf_len)
                                    };
                                    let mut pos = 0;
                                    for entry in entries {
                                        let name_bytes = entry.name.as_bytes();
                                        if pos + name_bytes.len() + 1 > buf_len { break; }
                                        buf[pos..pos + name_bytes.len()].copy_from_slice(name_bytes);
                                        pos += name_bytes.len();
                                        buf[pos] = 0;
                                        pos += 1;
                                    }
                                    tf.regs[10] = pos;
                                }
                                Err(_) => tf.regs[10] = usize::MAX,
                            }
                        } else {
                            tf.regs[10] = usize::MAX;
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                10 => { // sys_net_send
                    // arg0 = ptr, arg1 = len
                    let pkt = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Some(dev) = crate::net::device() {
                        dev.send(pkt);
                        tf.regs[10] = arg1; // bytes sent
                    } else {
                        tf.regs[10] = usize::MAX; // ENODEV
                    }
                }
                11 => { // sys_net_recv
                    // arg0 = buf ptr, arg1 = max_len
                    let buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1) };
                    if let Some(dev) = crate::net::device() {
                        let got = dev.recv(buf);
                        tf.regs[10] = got;
                    } else {
                        tf.regs[10] = usize::MAX; // ENODEV
                    }
                }
                40 => { // sys_socket -- create a proxied socket: pair a channel with net-daemon
                    // returns a user fd backed by a Channel; the net-daemon will pick up the peer via sys_daemon_next_socket
                    let (ep_user, ep_net) = crate::ipc::channel::ChannelEndpoint::create_pair();
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let user_fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep_user));

                    // Allocate socket id and register ownership
                    let sid = crate::net::socket::alloc_socket_id();
                    crate::net::socket::register_socket_owner(sid, crate::posix::process::current_pid(), user_fd);
                    crate::net::socket::push_new_socket(sid, ep_net);

                    tf.regs[10] = user_fd as usize;
                }
                41 => { // sys_daemon_next_socket -- net-daemon pulls next socket created by a user
                    if let Some((sid, ep)) = crate::net::socket::pop_new_socket() {
                        let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                        let fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep));
                        // pack sid (high 32) and fd (low 32) into return value
                        let ret = ((sid as usize) << 32) | (fd as usize);
                        tf.regs[10] = ret;
                    } else {
                        tf.regs[10] = usize::MAX; // no socket available
                    }
                }
                42 => { // sys_daemon_create_conn(listen_sid)
                    // arg0 = listen socket id
                    let listen_sid = arg0 as u32;
                    if let Some((owner_pid, _owner_fd)) = crate::net::socket::owner_of(listen_sid) {
                        // Create a channel pair to represent the accepted connection
                        let (user_ep, net_ep) = crate::ipc::channel::ChannelEndpoint::create_pair();

                        // Insert net_ep into daemon's fd table (return to daemon)
                        let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                        let net_fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(net_ep));

                        // Deliver user_ep into the owner process: either wake a blocked accept or queue it
                        crate::net::socket::push_pending_accepted(listen_sid, {
                            // insert into owner's fd table
                            let owner_proc = crate::posix::process::PROCESS_TABLE.lock().get(&owner_pid).cloned().unwrap();
                            owner_proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(user_ep))
                        });

                        // If the owner is currently blocked in accept, pop the waiting pid and wake it
                        if let Some(waiting_pid) = crate::net::socket::pop_waiting_pid(listen_sid) {
                            // Wake the waiting pid: ensure it's scheduled to run; the accept syscall will check pending_accepted
                            crate::posix::process::RUN_QUEUE.lock().push_back(waiting_pid);
                        }

                        tf.regs[10] = net_fd as usize;
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                43 => { // sys_accept (userland)
                    // arg0 = listen_fd
                    let listen_fd = arg0 as u32;
                    // Find socket id for this fd
                    if let Some(sid) = crate::net::socket::socket_id_for_fd(crate::posix::process::current_pid(), listen_fd) {
                        // If there's a pending accepted fd, return it
                        if let Some(fd) = crate::net::socket::pop_pending_accepted(sid) {
                            tf.regs[10] = fd as usize;
                        } else {
                            // Block and add to pending accepts
                            crate::net::socket::add_pending_accept(sid, crate::posix::process::current_pid());
                            tf.sepc -= 4; // rewind ecall
                            crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                            crate::posix::process::schedule();
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                44 => { // sys_bind(fd, addr_ptr, addr_len)
                    let fd = arg0 as u32;
                    let addr_ptr = arg1 as *const u8;
                    let addr_len = arg2 as usize;
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                // Copy addr bytes
                                let addr_bytes = unsafe { core::slice::from_raw_parts(addr_ptr, addr_len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_BIND);
                                v.extend_from_slice(addr_bytes);
                                let msg = crate::ipc::channel::Message { bytes: v, handles: alloc::vec::Vec::new() };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else { tf.regs[10] = usize::MAX }
                }
                45 => { // sys_listen(fd, backlog, _)
                    let fd = arg0 as u32;
                    let backlog = arg1 as u32;
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_LISTEN);
                                v.extend_from_slice(&backlog.to_le_bytes());
                                let msg = crate::ipc::channel::Message { bytes: v, handles: alloc::vec::Vec::new() };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else { tf.regs[10] = usize::MAX }
                }
                46 => { // sys_connect(fd, addr_ptr, addr_len)
                    let fd = arg0 as u32;
                    let addr_ptr = arg1 as *const u8;
                    let addr_len = arg2 as usize;
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let addr_bytes = unsafe { core::slice::from_raw_parts(addr_ptr, addr_len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_CONNECT);
                                v.extend_from_slice(addr_bytes);
                                let msg = crate::ipc::channel::Message { bytes: v, handles: alloc::vec::Vec::new() };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = 0,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else { tf.regs[10] = usize::MAX }
                }
                47 => { // sys_sock_send(fd, buf_ptr, len)
                    let fd = arg0 as u32;
                    let buf_ptr = arg1 as *const u8;
                    let len = arg2 as usize;
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                let data = unsafe { core::slice::from_raw_parts(buf_ptr, len) };
                                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                v.push(crate::net::socket::OPCODE_SEND);
                                v.extend_from_slice(data);
                                let msg = crate::ipc::channel::Message { bytes: v, handles: alloc::vec::Vec::new() };
                                match ep.write(msg) {
                                    Ok(_) => tf.regs[10] = len,
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else { tf.regs[10] = usize::MAX }
                }
                48 => { // sys_sock_recv(fd, buf_ptr, max_len)
                    let fd = arg0 as u32;
                    let buf_ptr = arg1 as *mut u8;
                    let max_len = arg2 as usize;
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    let fds = proc.fds.lock();
                    if let Some(obj) = fds.get(fd) {
                        match obj {
                            crate::ipc::handle::KernelObject::Channel(ep) => {
                                if let Some(msg) = ep.try_recv() {
                                    let to_copy = core::cmp::min(msg.bytes.len(), max_len);
                                    let dst = unsafe { core::slice::from_raw_parts_mut(buf_ptr, to_copy) };
                                    dst.copy_from_slice(&msg.bytes[..to_copy]);
                                    tf.regs[10] = to_copy;
                                } else {
                                    // Block until message arrives
                                    drop(fds);
                                    tf.sepc -= 4;
                                    crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                                    crate::posix::process::schedule();
                                }
                            }
                            _ => tf.regs[10] = usize::MAX,
                        }
                    } else { tf.regs[10] = usize::MAX }
                }
                23 => { // sys_setuid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    if proc.euid == 0 {
                        // Root can set UID to anything
                        unsafe {
                            let p_ptr = alloc::sync::Arc::as_ptr(&proc) as *mut crate::posix::process::Process;
                            (*p_ptr).uid = arg0 as u32;
                            (*p_ptr).euid = arg0 as u32;
                        }
                        tf.regs[10] = 0;
                    } else if arg0 as u32 == proc.uid {
                        // Non-root can set euid back to real uid
                        unsafe {
                            let p_ptr = alloc::sync::Arc::as_ptr(&proc) as *mut crate::posix::process::Process;
                            (*p_ptr).euid = arg0 as u32;
                        }
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX - 1; // EPERM
                    }
                }
                24 => { // sys_setgid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    if proc.euid == 0 {
                        unsafe {
                            let p_ptr = alloc::sync::Arc::as_ptr(&proc) as *mut crate::posix::process::Process;
                            (*p_ptr).gid = arg0 as u32;
                            (*p_ptr).egid = arg0 as u32;
                        }
                        tf.regs[10] = 0;
                    } else if arg0 as u32 == proc.gid {
                        unsafe {
                            let p_ptr = alloc::sync::Arc::as_ptr(&proc) as *mut crate::posix::process::Process;
                            (*p_ptr).egid = arg0 as u32;
                        }
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = usize::MAX - 1; // EPERM
                    }
                }
                26 => { // sys_create — create or truncate a file, return writable fd
                    let path_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    if let Ok(path) = core::str::from_utf8(path_bytes) {
                        match crate::vfs::lookup_parent(path) {
                            Ok((parent, name)) => {
                                match parent.create(&name) {
                                    Ok(vnode) => {
                                        let proc = crate::posix::process::PROCESS_TABLE.lock()
                                            .get(&crate::posix::process::current_pid()).cloned().unwrap();
                                        let fd_desc = crate::ipc::handle::FileDescription {
                                            vnode,
                                            offset: spin::Mutex::new(0),
                                        };
                                        let fd = proc.fds.lock().insert(
                                            crate::ipc::handle::KernelObject::File(
                                                alloc::sync::Arc::new(fd_desc)
                                            )
                                        );
                                        tf.regs[10] = fd as usize;
                                    }
                                    Err(_) => tf.regs[10] = usize::MAX,
                                }
                            }
                            Err(_) => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                30 => { // sys_getuid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    tf.regs[10] = proc.uid as usize;
                }
                31 => { // sys_geteuid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    tf.regs[10] = proc.euid as usize;
                }
                32 => { // sys_getgid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    tf.regs[10] = proc.gid as usize;
                }
                33 => { // sys_getegid
                    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                    tf.regs[10] = proc.egid as usize;
                }
                34 => { // sys_authenticate
                    // arg0=user_ptr, arg1=user_len, arg2=pass_ptr, a3=pass_len
                    let pass_len = tf.regs[13]; // a3
                    let username_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                    let password_bytes = unsafe { core::slice::from_raw_parts(arg2 as *const u8, pass_len) };
                    if let Ok(username) = core::str::from_utf8(username_bytes) {
                        match crate::posix::user::authenticate(username, password_bytes) {
                            Some(entry) => tf.regs[10] = entry.uid as usize,
                            None        => tf.regs[10] = usize::MAX,
                        }
                    } else {
                        tf.regs[10] = usize::MAX;
                    }
                }
                35 => { // sys_can_sudo
                    // arg0 = uid to check (or 0 = use current process uid)
                    let uid = if arg0 == 0 {
                        let proc = crate::posix::process::PROCESS_TABLE.lock().get(&crate::posix::process::current_pid()).cloned().unwrap();
                        proc.uid
                    } else {
                        arg0 as u32
                    };
                    tf.regs[10] = if crate::posix::user::can_sudo(uid) { 1 } else { 0 };
                }
                37 => { // sys_set_echo — arg0: 0=disable, 1=enable
                    *ECHO_ENABLED.lock() = arg0 != 0;
                    tf.regs[10] = 0;
                }
                38 => { // sys_set_raw — arg0: 1=raw (char-by-char), 0=cooked (line discipline)
                    *RAW_MODE.lock() = arg0 != 0;
                    tf.regs[10] = 0;
                }
                50 => { // sys_fork (scaffold -> COW)
                    match crate::posix::spawn::sys_fork() {
                        Ok(child_pid) => {
                            // In parent, return child's pid
                            tf.regs[10] = child_pid as usize;
                        }
                        Err(_) => {
                            tf.regs[10] = usize::MAX; // error for now
                        }
                    }
                }
                51 => { // sys_exec(path_ptr, len)
                    let path_ptr = arg0 as *const u8;
                    let path_len = arg1 as usize;
                    match crate::posix::spawn::sys_exec(path_ptr, path_len) {
                        Ok(()) => { tf.regs[10] = 0; }
                        Err(_) => { tf.regs[10] = usize::MAX; }
                    }
                }
                52 => { // sys_waitpid(target_pid, status_ptr, options)
                    let target = arg0 as i32; // pid or -1 for any
                    let status_ptr = arg1 as *mut i32;

                    // First, if the current process already has a delivered wait_result, consume it
                    let ppid = crate::posix::process::current_pid();
                    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&ppid) {
                        if let Some((zpid, zstatus)) = proc.wait_result.lock().take() {
                            // Reap the child (remove from process table and parent children list)
                            let mut table = crate::posix::process::PROCESS_TABLE.lock();
                            table.remove(&zpid);
                            if let Some(parent) = table.get(&ppid) {
                                parent.children.lock().retain(|&p| p != zpid);
                            }

                            // Write status to user pointer if provided
                            if !status_ptr.is_null() {
                                unsafe { *(status_ptr) = zstatus as i32; }
                            }
                            tf.regs[10] = zpid as usize;
                            // proceed to return below
                        } else {
                            // No delivered result yet: check for existing zombies among children
                            let mut found_zombie = None;
                            {
                                let table = crate::posix::process::PROCESS_TABLE.lock();
                                if let Some(parent) = table.get(&ppid) {
                                    let children = parent.children.lock();
                                    for &child_pid in children.iter() {
                                        if target == -1 || target == child_pid {
                                            if let Some(child) = table.get(&child_pid) {
                                                let state = child.state.lock();
                                                if let crate::posix::process::ProcState::Zombie(status) = *state {
                                                    found_zombie = Some((child_pid, status));
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some((zpid, zstatus)) = found_zombie {
                                // Reap immediately
                                let mut table = crate::posix::process::PROCESS_TABLE.lock();
                                table.remove(&zpid);
                                if let Some(parent) = table.get(&ppid) {
                                    parent.children.lock().retain(|&p| p != zpid);
                                }
                                if !status_ptr.is_null() {
                                    unsafe { *(status_ptr) = zstatus as i32; }
                                }
                                tf.regs[10] = zpid as usize;
                            } else {
                                // Block: register interest and schedule away
                                if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&ppid) {
                                    *proc.wait_target.lock() = Some(target);
                                    *proc.wait_status_ptr.lock() = Some(status_ptr as usize);
                                    // Rewind ecall so syscall restarts when woken
                                    tf.sepc -= 4;
                                    // Mark this process as not runnable and yield
                                    *proc.state.lock() = crate::posix::process::ProcState::Stopped;
                                    crate::posix::process::RUN_QUEUE.lock().push_back(ppid);
                                    crate::posix::process::schedule();
                                } else {
                                    tf.regs[10] = usize::MAX; // parent not found
                                }
                            }
                        }
                    } else {
                        tf.regs[10] = usize::MAX; // current proc not found
                    }
                }
                _ => {
                    panic!("Unknown syscall {}", syscall_num);
                }
            }
            
            // Advance sepc to next instruction after ecall
            tf.sepc += 4;
            tf as *mut _
        }
        // Handle store/AMO page fault for copy-on-write
        scause::Trap::Exception(15) => {
            use riscv::register::stval;
            let fault_va = stval::read();
            crate::println!("Store page fault at {:#x}, attempting COW", fault_va);
            match crate::mm::vmm::handle_store_page_fault(fault_va) {
                Ok(()) => {
                    // Resume execution; do not advance sepc here since faulting instruction must retry
                    tf as *mut _
                }
                Err(e) => {
                    panic!("Unhandled store page fault: {}", e);
                }
            }
        }
        scause::Trap::Interrupt(_) => {
            // External interrupt: claim from PLIC and dispatch
            let hart = riscv::register::mhartid::read();
            let irq = crate::plic::claim(hart as usize);
            if irq != 0 {
                crate::net::virtio_mmio::handle_interrupt(irq);
                crate::plic::complete(hart as usize, irq);
            }
            tf as *mut _
        }
        _ => {
            panic!("Unhandled trap: {:?}, sepc: {:#x}", cause, tf.sepc);
        }
    }
}
