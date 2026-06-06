#![no_std]
#![no_main]

extern crate alloc;

use libos::ipc::{Message};
use libos::{msg_receive, msg_send, msg_sendrec, IPC_ANY, write, exit, fork, waitpid, getpid};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let _pid = getpid();
    wrt(b"ipc_test: starting\n");

    let child = fork();
    if child == 0 {
        // Child
        let _child_pid = getpid();
        wrt(b"ipc_test_child: ready\n");

        // Receive message from anywhere
        let mut msg = Message::new();
        let _res = msg_receive(IPC_ANY, &mut msg);
        wrt(b"ipc_test_child: received msg\n");

        // Send a reply
        let mut reply = Message::new();
        reply.type_ = 42;
        reply.data[0] = 99;
        let _res = msg_send(msg.source, &reply);
        wrt(b"ipc_test_child: sent reply\n");

        exit(0);
    } else if child > 0 {
        // Parent
        wrt(b"ipc_test: waiting a bit for child to be ready\n");
        for _ in 0..1000000 { core::hint::spin_loop(); }

        let mut msg = Message::new();
        msg.type_ = 1;
        msg.data[0] = 77;
        
        wrt(b"ipc_test: sending sendrec to child\n");
        let _res = msg_sendrec(child, &mut msg);
        
        wrt(b"ipc_test: sendrec returned\n");

        if msg.type_ == 42 && msg.data[0] == 99 {
            wrt(b"ipc_test: PASS\n");
        } else {
            wrt(b"ipc_test: FAIL\n");
        }

        let mut status = 0;
        waitpid(child, &mut status, 0);
        return 0;
    } else {
        wrt(b"ipc_test: fork failed\n");
        return 1;
    }
}
