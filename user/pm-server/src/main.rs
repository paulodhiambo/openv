#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use libos::{msg_receive, msg_send};
use libos::ipc::Message;
use pm_proto::*;

fn wrt(s: &str) {
    libos::syscall(2, 1, s.as_ptr() as usize, s.len());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcState {
    Running,
    Zombie(i32),
}

struct Process {
    #[allow(dead_code)]
    pid: i32,
    ppid: i32,
    state: ProcState,
    children: Vec<i32>,
    waiting_parent: bool,
    wait_target: i32,
}

static mut PM_TABLE: Option<BTreeMap<i32, Process>> = None;

#[allow(static_mut_refs)]
fn get_table() -> &'static mut BTreeMap<i32, Process> {
    unsafe {
        if PM_TABLE.is_none() {
            PM_TABLE = Some(BTreeMap::new());
        }
        PM_TABLE.as_mut().unwrap()
    }
}

#[no_mangle]
pub fn main() {
    wrt("PM server started.\n");
    
    // PM is PID 2. Register init (PID 1).
    let table = get_table();
    table.insert(1, Process {
        pid: 1,
        ppid: 0,
        state: ProcState::Running,
        children: Vec::new(),
        waiting_parent: false,
        wait_target: 0,
    });
    table.insert(2, Process {
        pid: 2,
        ppid: 0,
        state: ProcState::Running,
        children: Vec::new(),
        waiting_parent: false,
        wait_target: 0,
    });

    loop {
        let mut msg = Message::new();
        let sender = msg_receive(-1, &mut msg);
        if sender < 0 { continue; }

        match msg.type_ {
            OP_FORK => {
                wrt("PM: handling OP_FORK\n");
                handle_fork(sender, &mut msg);
            }
            OP_EXEC => handle_exec(sender, &mut msg),
            OP_WAITPID => handle_waitpid(sender, &mut msg),
            OP_EXIT => {
                wrt("PM: handling OP_EXIT\n");
                handle_exit(sender, &mut msg);
            }
            _ => {
                let mut reply = Message::new();
                reply.type_ = REPLY_ERR;
                msg_send(sender, &reply);
            }
        }
    }
}

fn handle_fork(sender: i32, _msg: &mut Message) {
    ensure_process_exists(sender);
    let new_pid = libos::syscall(100, sender as usize, 0, 0) as i32;
    if new_pid < 0 {
        wrt("PM: fork failed to clone process\n");
        let mut reply = Message::new();
        reply.type_ = REPLY_ERR;
        msg_send(sender, &reply);
        return;
    }

    let table = get_table();
    table.insert(new_pid, Process {
        pid: new_pid,
        ppid: sender,
        state: ProcState::Running,
        children: Vec::new(),
        waiting_parent: false,
        wait_target: 0,
    });
    if let Some(parent) = table.get_mut(&sender) {
        parent.children.push(new_pid);
    }

    // Reply to parent
    let mut p_msg = Message::new();
    p_msg.type_ = REPLY_OK;
    pack_reply_u32(&mut p_msg.data, new_pid as u32);
    msg_send(sender, &p_msg);
    wrt("PM: sent reply to parent\n");

    // Reply to child
    let mut c_msg = Message::new();
    c_msg.type_ = REPLY_OK;
    pack_reply_u32(&mut c_msg.data, 0);
    msg_send(new_pid, &c_msg);
    wrt("PM: sent reply to child\n");
}

fn handle_waitpid(sender: i32, msg: &mut Message) {
    wrt("PM: handling OP_WAITPID\n");
    ensure_process_exists(sender);
    let (target, options) = unpack_waitpid_req(&msg.data);
    let table = get_table();
    
    let mut found_zombie = None;
    if let Some(parent) = table.get(&sender) {
        for &child_pid in &parent.children {
            if target == -1 || target == child_pid {
                if let Some(child) = table.get(&child_pid) {
                    if let ProcState::Zombie(st) = child.state {
                        found_zombie = Some((child_pid, st));
                        break;
                    }
                }
            }
        }
    }

    if let Some((zpid, zstatus)) = found_zombie {
        wrt("PM: waitpid found zombie\n");
        reap_process(zpid);
        wrt("PM: waitpid reaped zombie\n");
        let mut rep = Message::new();
        rep.type_ = REPLY_OK;
        pack_reply_u32(&mut rep.data, zpid as u32);
        rep.data[4..8].copy_from_slice(&zstatus.to_le_bytes());
        msg_send(sender, &rep);
        wrt("PM: waitpid sent reply to parent\n");
    } else {
        if options & 1 != 0 { // WNOHANG
            let mut rep = Message::new();
            rep.type_ = REPLY_OK;
            pack_reply_u32(&mut rep.data, 0);
            msg_send(sender, &rep);
        } else {
            if let Some(parent) = table.get_mut(&sender) {
                parent.waiting_parent = true;
                parent.wait_target = target;
                wrt("PM: waitpid blocking parent\n");
            }
        }
    }
}

fn handle_exit(sender: i32, msg: &mut Message) {
    wrt("PM: handling OP_EXIT\n");
    ensure_process_exists(sender);
    let status = unpack_exit_req(&msg.data);
    let table = get_table();
    
    if let Some(proc) = table.get_mut(&sender) {
        proc.state = ProcState::Zombie(status);
        let ppid = proc.ppid;
        
        // Orphan reparenting
        let children = proc.children.clone();
        for child_pid in children {
            if let Some(child) = table.get_mut(&child_pid) {
                child.ppid = 1; // init
            }
            if let Some(init) = table.get_mut(&1) {
                init.children.push(child_pid);
            }
        }
        
        // Check if parent is waiting
        if let Some(parent) = table.get_mut(&ppid) {
            if parent.waiting_parent && (parent.wait_target == -1 || parent.wait_target == sender) {
                parent.waiting_parent = false;
                let mut rep = Message::new();
                rep.type_ = REPLY_OK;
                pack_reply_u32(&mut rep.data, sender as u32);
                rep.data[4..8].copy_from_slice(&status.to_le_bytes());
                wrt("PM: sending waitpid reply\n");
                msg_send(ppid, &rep);
                wrt("PM: waitpid reply sent, reaping\n");
                reap_process(sender);
                wrt("PM: process reaped\n");
                return;
            }
        }
    }
}

fn reap_process(pid: i32) {
    let table = get_table();
    if let Some(proc) = table.remove(&pid) {
        let ppid = proc.ppid;
        if let Some(parent) = table.get_mut(&ppid) {
            parent.children.retain(|&c| c != pid);
        }
        libos::syscall(105, pid as usize, 0, 0); // sys_reap_process
    }
}

fn handle_exec(sender: i32, msg: &mut Message) {
    // Basic fallback to kernel sys_exec for now to avoid the massive VFS copy refactor in this step.
    let path = unpack_exec_req(&msg.data);
    let ret = libos::syscall(51, path.as_ptr() as usize, path.len(), 0);
    
    let mut rep = Message::new();
    if ret == usize::MAX {
        rep.type_ = REPLY_ERR;
    } else {
        rep.type_ = REPLY_OK;
    }
    msg_send(sender, &rep);
}

fn ensure_process_exists(pid: i32) {
    let table = get_table();
    if !table.contains_key(&pid) {
        table.insert(pid, Process {
            pid,
            ppid: 1, // default parent is init
            state: ProcState::Running,
            children: alloc::vec::Vec::new(),
            waiting_parent: false,
            wait_target: 0,
        });
        if pid != 1 {
            if let Some(init) = table.get_mut(&1) {
                init.children.push(pid);
            }
        }
    }
}
