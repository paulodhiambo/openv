use crate::trap::TrapFrame;

unsafe extern "C" {
    pub(crate) fn __halt_cpu() -> !;
}

pub fn sys_pipe(arg0: usize, tf: &mut TrapFrame) {
    let (rh, wh) = crate::ipc::handle::create_pipe();
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let mut fds = proc.fds.lock();
    let h_r = {
        let h = fds.lowest_free();
        fds.insert_at(h, crate::ipc::handle::KernelObject::PipeRead(rh));
        h
    };
    let h_w = {
        let h = fds.lowest_free();
        fds.insert_at(h, crate::ipc::handle::KernelObject::PipeWrite(wh));
        h
    };

    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const [u32; 2], 1) {
        tf.regs[10] = usize::MAX;
        return;
    }

    unsafe {
        let arr = arg0 as *mut [u32; 2];
        (*arr)[0] = h_r;
        (*arr)[1] = h_w;
    }
    tf.regs[10] = 0;
}

pub fn sys_dup(arg0: usize, tf: &mut TrapFrame) {
    let old_fd = arg0 as u32;
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let mut fds = proc.fds.lock();
    if let Some(obj) = fds.get(old_fd) {
        let obj_clone = obj.clone();
        let new_fd = fds.insert(obj_clone);
        tf.regs[10] = new_fd as usize;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_dup2(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let old_fd = arg0 as u32;
    let new_fd = arg1 as u32;
    if old_fd == new_fd {
        tf.regs[10] = new_fd as usize;
        return;
    }
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let mut fds = proc.fds.lock();
    if let Some(obj) = fds.get(old_fd) {
        let obj_clone = obj.clone();
        fds.insert_at(new_fd, obj_clone);
        tf.regs[10] = new_fd as usize;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_mailbox_send(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let ptr = arg1 as *const u8;
    let len = arg2;

    if len > 4096 {
        tf.regs[10] = usize::MAX;
        return;
    }

    // Add bounds checking for ptr
    if !crate::mm::vmm::is_user_pointer_valid(tf, ptr, len) {
        tf.regs[10] = usize::MAX;
        return;
    }

    let mut buf = alloc::vec![0u8; len];
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), len);
    }

    let sender_pid = crate::posix::process::current_pid();
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(target) = table.get(&to_pid) {
        target.mailbox.lock().push_back((sender_pid, buf));
        if target.mailbox_waiter.load(core::sync::atomic::Ordering::Relaxed) != 0 {
            target.mailbox_waiter.store(0, core::sync::atomic::Ordering::Relaxed);
            let mut state = target.state.lock();
            if matches!(*state, crate::posix::process::ProcState::Stopped) {
                *state = crate::posix::process::ProcState::Running;
                crate::posix::process::RUN_QUEUE.lock().push_back(to_pid);
            }
        }
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_mailbox_receive(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let ptr = arg0 as *mut u8;
    let max_len = arg1;
    let from_ptr = arg2 as *mut i32;

    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    
    let msg = {
        let mut mb = proc.mailbox.lock();
        mb.pop_front()
    };
    
    if let Some((sender, buf)) = msg {
        let copy_len = core::cmp::min(max_len, buf.len());

        // Add bounds checking for ptr
        if !crate::mm::vmm::is_user_pointer_valid(tf, ptr, copy_len) {
            tf.regs[10] = usize::MAX;
            return;
        }

        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, copy_len);
            if !from_ptr.is_null() {
                *from_ptr = sender;
            }
        }
        tf.regs[10] = copy_len as usize;
    } else {
        proc.mailbox_waiter.store(1, core::sync::atomic::Ordering::Relaxed);
        *proc.state.lock() = crate::posix::process::ProcState::Stopped;
        tf.sepc -= 4; // Re-execute syscall when woken up
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}


pub fn kernel_send_msg(target_pid: i32, msg: crate::ipc::msg::Message) {
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(target) = table.get(&target_pid) {
        let mut target_state = target.ipc_state.lock();
        match *target_state {
            crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } if source == msg.source || source == -1 => {
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg };
                crate::posix::process::RUN_QUEUE.lock().push_back(target_pid);
            }
            crate::posix::process::IpcState::None => {
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg };
            }
            _ => {
                // Target already has a message or is mid-IPC — queue for later delivery
                // so we don't silently drop rapid back-to-back interrupts.
                target.irq_pending.lock().push_back(msg);
            }
        }
    }
}

pub fn sys_ipc_send(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let msg_ptr = arg1;
    let sender_pid = crate::posix::process::current_pid();

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let sender = table.get(&sender_pid).unwrap();
    
    // Check if we are resuming from a successful send
    {
        let mut state = sender.ipc_state.lock();
        if let crate::posix::process::IpcState::SendComplete = *state {
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = 0; // Success
            return;
        }
    }

    // Read message from userspace
    let mut local_msg: crate::ipc::msg::Message = unsafe { core::mem::zeroed() };

    // Add bounds checking for msg_ptr
    if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *const crate::ipc::msg::Message, 1) {
        tf.regs[10] = usize::MAX;
        return;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(msg_ptr as *const crate::ipc::msg::Message, &mut local_msg, 1);
    }
    local_msg.source = sender_pid;

    if let Some(target) = table.get(&to_pid) {
        let mut target_state = target.ipc_state.lock();
        match *target_state {
            crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } |
            crate::posix::process::IpcState::ReceivingReply { source, msg_ptr: _ } if source == sender_pid || source == -1 => {
                // Target is waiting. Give them the message.
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg: local_msg };
                crate::posix::process::RUN_QUEUE.lock().push_back(to_pid);
                tf.regs[10] = 0; // Success immediately, no need to block
            }
            _ => {
                crate::println!("sys_ipc_send blocking! target_pid={}, target_state={:?}", to_pid, *target_state);
                // Target not waiting. Block ourselves.
                *sender.ipc_state.lock() = crate::posix::process::IpcState::Sending {
                    target: to_pid,
                    msg: local_msg,
                    reply_msg_ptr: None,
                };
                target.senders.lock().push_back(sender_pid);
                
                tf.sepc -= 4; // Re-execute when woken up
                drop(target_state);
                drop(table);
                crate::posix::process::schedule();
                unsafe { __halt_cpu() }
            }
        }
    } else {
        crate::println!("sys_ipc_send: target_pid={} not found in table!", to_pid);
        tf.regs[10] = usize::MAX; // ESRCH
    }
}



pub fn sys_ipc_receive(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let from_pid = arg0 as i32;
    let msg_ptr = arg1;
    let receiver_pid = crate::posix::process::current_pid();

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let receiver = table.get(&receiver_pid).unwrap();
    
    // Check if we have a message available (either from a previous wake or deposited by a sender)
    {
        let mut state = receiver.ipc_state.lock();
        if let crate::posix::process::IpcState::MessageAvailable { msg } = *state {
            let sender_pid = msg.source;

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            // Promote the next queued IRQ message (if any) so it isn't lost
            let next = receiver.irq_pending.lock().pop_front();
            *state = match next {
                Some(next_msg) => crate::posix::process::IpcState::MessageAvailable { msg: next_msg },
                None => crate::posix::process::IpcState::None,
            };
            tf.regs[10] = sender_pid as usize; // Return sender PID
            return;
        }
    }
    // Also drain irq_pending before we scan senders or block, so IRQs that
    // arrived while we were in a non-receivable state are not missed.
    {
        let mut pending = receiver.irq_pending.lock();
        if let Some(pos) = pending.iter().position(|m| from_pid == -1 || from_pid == m.source) {
            let msg = pending.remove(pos).unwrap();
            let sender_pid = msg.source;

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            tf.regs[10] = sender_pid as usize;
            return;
        }
    }
    
    let mut senders = receiver.senders.lock();
    
    // Find a matching sender
    let mut found_idx = None;
    for (i, &sender_pid) in senders.iter().enumerate() {
        if from_pid == -1 || from_pid == sender_pid {
            found_idx = Some(i);
            break;
        }
    }
    
    if let Some(idx) = found_idx {
        // We have a pending sender!
        let sender_pid = senders.remove(idx).unwrap();
        let sender = table.get(&sender_pid).unwrap();
        let mut sender_state = sender.ipc_state.lock();
        
        if let crate::posix::process::IpcState::Sending { msg, reply_msg_ptr, .. } = *sender_state {
            let sender_pid_actual = msg.source; // Should be sender_pid

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            // Copy message to our userspace
            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            
            if let Some(r_ptr) = reply_msg_ptr {
                // Sender was using sendrec, transition them to waiting for reply
                *sender_state = crate::posix::process::IpcState::ReceivingReply {
                    source: receiver_pid,
                    msg_ptr: r_ptr,
                };
                // Do NOT wake them yet, they wait for reply!
            } else {
                // Sender was using send, they are done
                *sender_state = crate::posix::process::IpcState::SendComplete;
                crate::posix::process::RUN_QUEUE.lock().push_back(sender_pid);
            }
            
            tf.regs[10] = sender_pid_actual as usize; // Return sender PID
        } else {
            tf.regs[10] = usize::MAX; // Should not happen
        }
    } else {
        // No pending sender. Block receiver.
        *receiver.ipc_state.lock() = crate::posix::process::IpcState::Receiving {
            source: from_pid,
            msg_ptr,
        };
        tf.sepc -= 4; // Re-execute when woken up
        drop(senders);
        drop(table);
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}


pub fn sys_ipc_sendrec(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let msg_ptr = arg1;

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let sender_pid = crate::posix::process::current_pid();
    let sender = table.get(&sender_pid).unwrap();
    
    // Check our current state to see where we are in the transaction
    let mut state = sender.ipc_state.lock();
    
    match *state {
        crate::posix::process::IpcState::MessageAvailable { msg } => {
            // We received the reply! Copy to userspace and finish.
            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = 0;
        }
        crate::posix::process::IpcState::ReceivingReply { .. } => {
            // Spurious wakeup, go back to sleep
            tf.sepc -= 4;
            drop(state);
            drop(table);
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        crate::posix::process::IpcState::None => {
            // Initial call: we need to send the message
            let mut local_msg: crate::ipc::msg::Message = unsafe { core::mem::zeroed() };
            unsafe {
                core::ptr::copy_nonoverlapping(msg_ptr as *const crate::ipc::msg::Message, &mut local_msg, 1);
            }
            local_msg.source = sender_pid;

            if let Some(target) = table.get(&to_pid) {
                let mut target_state = target.ipc_state.lock();
                match *target_state {
                    crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } if source == sender_pid || source == -1 => {
                        // Target is ready. Deposit message and transition to ReceivingReply.
                        *target_state = crate::posix::process::IpcState::MessageAvailable { msg: local_msg };
                        crate::posix::process::RUN_QUEUE.lock().push_back(to_pid);
                        
                        *state = crate::posix::process::IpcState::ReceivingReply {
                            source: to_pid,
                            msg_ptr,
                        };
                        tf.sepc -= 4; // Block to wait for reply
                        drop(target_state);
                        drop(state);
                        drop(table);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                    _ => {
                        // Target not ready. Block and wait to send.
                        *state = crate::posix::process::IpcState::Sending {
                            target: to_pid,
                            msg: local_msg,
                            reply_msg_ptr: Some(msg_ptr),
                        };
                        target.senders.lock().push_back(sender_pid);
                        
                        tf.sepc -= 4; // Block to wait for send AND reply
                        drop(target_state);
                        drop(state);
                        drop(table);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
            } else {
                tf.regs[10] = usize::MAX; // Target doesn't exist
            }
        }
        _ => {
            // Inconsistent state
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = usize::MAX;
        }
    }
}

pub const F_DUPFD: usize = 0;
pub const F_GETFD: usize = 1;
pub const F_SETFD: usize = 2;
pub const F_GETFL: usize = 3;
pub const F_SETFL: usize = 4;
pub const F_SET_VFS_FD: usize = 5;
pub const F_GET_VFS_FD: usize = 6;
pub const FD_CLOEXEC: usize = 1;

pub fn sys_fcntl(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let mut fds = proc.fds.lock();
    let fd = arg0 as u32;

    if arg1 != F_SET_VFS_FD && !fds.get(fd).is_some() {
        tf.regs[10] = usize::MAX;
        return;
    }

    match arg1 {
        F_DUPFD => {
            let min_fd = arg2 as u32;
            let obj = fds.get(fd).unwrap().clone();
            // find lowest free fd >= min_fd
            let mut newfd = min_fd;
            while fds.get(newfd).is_some() {
                newfd += 1;
            }
            fds.insert_at(newfd, obj);
            tf.regs[10] = newfd as usize;
        }
        F_GETFD => {
            tf.regs[10] = if fds.is_cloexec(fd) { FD_CLOEXEC } else { 0 };
        }
        F_SETFD => {
            fds.set_cloexec(fd, (arg2 & FD_CLOEXEC) != 0);
            tf.regs[10] = 0;
        }
        F_GETFL => {
            tf.regs[10] = 0; // we don't have file status flags yet
        }
        F_SETFL => {
            tf.regs[10] = 0; // ignore for now
        }
        F_SET_VFS_FD => {
            let server_fd = arg2 as u32;
            if fd == u32::MAX {
                let newfd = fds.insert(crate::ipc::handle::KernelObject::VfsFile(server_fd));
                tf.regs[10] = newfd as usize;
            } else {
                fds.insert_at(fd, crate::ipc::handle::KernelObject::VfsFile(server_fd));
                tf.regs[10] = 0;
            }
        }
        F_GET_VFS_FD => {
            let obj = fds.get(fd).unwrap();
            if let crate::ipc::handle::KernelObject::VfsFile(server_fd) = obj {
                tf.regs[10] = *server_fd as usize;
            } else {
                tf.regs[10] = usize::MAX;
            }
        }
        _ => {
            tf.regs[10] = usize::MAX;
        }
    }
}

pub fn sys_datacopy(
    src_pid: usize,
    src_va: usize,
    dst_pid: usize,
    dst_va: usize,
    len: usize,
    tf: &mut TrapFrame,
) {
    let proc = crate::posix::process::get_current_proc().unwrap();
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_DATACOPY == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let src_pid = src_pid as i32;
    let dst_pid = dst_pid as i32;

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let src_proc = match table.get(&src_pid) {
        Some(p) => p,
        None => {
            tf.regs[10] = usize::MAX; // ESRCH
            return;
        }
    };
    let dst_proc = match table.get(&dst_pid) {
        Some(p) => p,
        None => {
            tf.regs[10] = usize::MAX; // ESRCH
            return;
        }
    };

    let src_root_pa = (src_proc.satp_val.load(core::sync::atomic::Ordering::Acquire) & 0xFFF_FFFF_FFFF) << 12;
    let dst_root_pa = (dst_proc.satp_val.load(core::sync::atomic::Ordering::Acquire) & 0xFFF_FFFF_FFFF) << 12;

    // Drop the lock before we start memory copying to avoid holding it during page faults or long copies
    drop(table);

    let mut copied = 0;
    while copied < len {
        let current_src_va = src_va + copied;
        let current_dst_va = dst_va + copied;

        // Try to resolve source PA
        let src_pa = match crate::mm::vmm::PageTable::walk_page_table(src_root_pa, current_src_va) {
            Ok((pa, flags)) => {
                if flags & crate::mm::vmm::PTE_R == 0 {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                pa
            }
            Err(_) => {
                // Not mapped, try to fault it in
                if crate::mm::vmm::handle_user_page_fault(src_root_pa, current_src_va, 0).is_err() {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                // Retry walk
                match crate::mm::vmm::PageTable::walk_page_table(src_root_pa, current_src_va) {
                    Ok((pa, _)) => pa,
                    Err(_) => {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                }
            }
        };

        // Try to resolve destination PA
        let dst_pa = match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
            Ok((pa, flags)) => {
                if flags & crate::mm::vmm::PTE_W == 0 {
                    // Try COW
                    if crate::mm::vmm::handle_store_page_fault(dst_root_pa, current_dst_va).is_err() {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                    match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
                        Ok((pa, _)) => pa,
                        Err(_) => {
                            tf.regs[10] = usize::MAX; // EFAULT
                            return;
                        }
                    }
                } else {
                    pa
                }
            }
            Err(_) => {
                // Not mapped, try to fault it in
                if crate::mm::vmm::handle_user_page_fault(dst_root_pa, current_dst_va, 0).is_err() {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                // Retry walk
                match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
                    Ok((pa, _)) => pa,
                    Err(_) => {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                }
            }
        };

        // Calculate how many bytes we can copy in this chunk (up to page boundary for both)
        let src_page_offset = current_src_va % crate::mm::pmm::PAGE_SIZE;
        let dst_page_offset = current_dst_va % crate::mm::pmm::PAGE_SIZE;
        let src_rem = crate::mm::pmm::PAGE_SIZE - src_page_offset;
        let dst_rem = crate::mm::pmm::PAGE_SIZE - dst_page_offset;
        let chunk = core::cmp::min(len - copied, core::cmp::min(src_rem, dst_rem));

        unsafe {
            core::ptr::copy_nonoverlapping(src_pa as *const u8, dst_pa as *mut u8, chunk);
        }

        copied += chunk;
    }

    tf.regs[10] = copied;
}
