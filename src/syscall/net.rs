use crate::trap::TrapFrame;

unsafe extern "C" {
    fn __halt_cpu() -> !;
}

pub fn sys_net_send(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_NET_RAW == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const u8, arg1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let pkt = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
    if let Some(dev) = crate::net::device() {
        dev.send(pkt);
        tf.regs[10] = arg1;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_net_recv(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_NET_RAW == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const u8, arg1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1) };
    if let Some(dev) = crate::net::device() {
        let got = dev.recv(buf);
        tf.regs[10] = got;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_socket(tf: &mut TrapFrame) {
    let (ep_user, ep_net) = crate::ipc::channel::ChannelEndpoint::create_pair();
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let user_fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep_user));

    let sid = crate::net::socket::alloc_socket_id();
    crate::net::socket::register_socket_owner(sid, crate::posix::process::current_pid(), user_fd);
    crate::net::socket::push_new_socket(sid, ep_net);

    tf.regs[10] = user_fd as usize;
}

pub fn sys_daemon_next_socket(tf: &mut TrapFrame) {
    if let Some((sid, ep)) = crate::net::socket::pop_new_socket() {
        crate::get_current_proc_or_esrch!(tf);
        let proc = crate::posix::process::get_current_proc().unwrap();
        let fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(ep));
        let ret = ((sid as usize) << 32) | (fd as usize);
        tf.regs[10] = ret;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_daemon_create_conn(arg0: usize, tf: &mut TrapFrame) {
    let listen_sid = arg0 as u32;
    if let Some((owner_pid, _owner_fd)) = crate::net::socket::owner_of(listen_sid) {
        let (user_ep, net_ep) = crate::ipc::channel::ChannelEndpoint::create_pair();

        crate::get_current_proc_or_esrch!(tf);
        let proc = crate::posix::process::get_current_proc().unwrap();
        let net_fd = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(net_ep));

        crate::net::socket::push_pending_accepted(listen_sid, {
            let owner_proc = match crate::posix::process::PROCESS_TABLE.lock().get(&owner_pid).cloned() {
                Some(p) => p,
                None => {
                    tf.regs[10] = crate::errno::ESRCH;
                    tf.sepc += 4;
                    return;
                }
            };
            owner_proc.fds.lock().insert(crate::ipc::handle::KernelObject::Channel(user_ep))
        });

        if let Some(waiting_pid) = crate::net::socket::pop_waiting_pid(listen_sid) {
            crate::posix::process::RUN_QUEUE.lock().push_back(waiting_pid);
        }

        tf.regs[10] = net_fd as usize;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_accept(arg0: usize, tf: &mut TrapFrame) {
    let listen_fd = arg0 as u32;
    if let Some(sid) = crate::net::socket::socket_id_for_fd(crate::posix::process::current_pid(), listen_fd) {
        if let Some(fd) = crate::net::socket::pop_pending_accepted(sid) {
            tf.regs[10] = fd as usize;
        } else {
            crate::net::socket::add_pending_accept(sid, crate::posix::process::current_pid());
            tf.sepc -= 4;
            crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_bind(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let addr_ptr = arg1 as *const u8;
    let addr_len = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, addr_ptr, addr_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let fds = proc.fds.lock();
    if let Some(obj) = fds.get(fd) {
        match obj {
            crate::ipc::handle::KernelObject::Channel(ep) => {
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

pub fn sys_listen(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let backlog = arg1 as u32;
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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

pub fn sys_connect(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let addr_ptr = arg1 as *const u8;
    let addr_len = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, addr_ptr, addr_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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

pub fn sys_sock_send(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let buf_ptr = arg1 as *const u8;
    let len = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr, len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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

pub fn sys_sock_recv(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let buf_ptr = arg1 as *mut u8;
    let max_len = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, max_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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
                    drop(fds);
                    drop(proc);
                    tf.sepc -= 4;
                    crate::posix::process::RUN_QUEUE.lock().push_back(crate::posix::process::current_pid());
                    crate::posix::process::schedule();
                    unsafe { __halt_cpu() }
                }
            }
            _ => tf.regs[10] = usize::MAX,
        }
    } else { tf.regs[10] = usize::MAX }
}

pub fn sys_try_recv(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let fd = arg0 as u32;
    let buf_ptr = arg1 as *mut u8;
    let max_len = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, max_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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
                    tf.regs[10] = 0;
                }
            }
            _ => tf.regs[10] = usize::MAX,
        }
    } else { tf.regs[10] = usize::MAX; }
}
