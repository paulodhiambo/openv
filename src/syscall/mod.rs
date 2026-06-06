use crate::trap::TrapFrame;

#[macro_export]
macro_rules! get_current_proc_or_esrch {
    ($tf:expr) => {
        match crate::posix::process::get_current_proc() {
            Some(p) => p,
            None => {
                $tf.regs[10] = crate::errno::ESRCH;
                $tf.sepc += 4;
                return;
            }
        }
    };
}

pub mod fs;
pub mod ipc;
pub mod net;
pub mod proc;
pub mod tty;
pub mod user;
pub mod pm;

pub fn dispatch(
    syscall_num: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    tf: &mut TrapFrame,
) {
    match syscall_num {
        // pm.rs primitives
        100 => pm::sys_clone_process(arg0, tf),
        101 => pm::sys_set_trapframe(arg0, arg1, tf),
        102 => pm::sys_set_process_state(arg0, arg1, tf),
        103 => pm::sys_destroy_user_space(arg0, tf),
        104 => pm::sys_alloc_user_page(arg0, arg1, arg2, tf),
        105 => pm::sys_reap_process(arg0, tf),
        106 => pm::sys_phys_map(arg0, arg1, arg2, tf),
        107 => proc::sys_irq_register(arg0, arg1, tf),
        108 => proc::sys_irq_enable(arg0, tf),
        109 => pm::sys_virt_to_phys(arg0, tf),

        // proc.rs
        0 => proc::sys_yield(tf),
        1 => proc::sys_exit(arg0, tf),
        6 => proc::sys_spawn(arg0, arg1, tf),
        7 => proc::sys_spawn_with_caps(arg0, arg1, arg2 as u64, tf),
        50 => proc::sys_fork(tf),
        51 => proc::sys_exec(arg0, arg1, arg2, arg3, tf),
        52 => proc::sys_waitpid(arg0, arg1, arg2, tf),
        53 => proc::sys_getpid(tf),
        54 => proc::sys_getppid(tf),
        60 => proc::sys_set_fg_pid(arg0, tf),
        82 => proc::sys_gettimeofday(arg0, arg1, tf),
        83 => proc::sys_nanosleep(arg0, arg1, tf),
        59 => proc::sys_privctl(arg0, arg1 as u64, tf),
        84 => proc::sys_setpgid(arg0, arg1, tf),
        85 => proc::sys_getpgid(arg0, tf),
        86 => proc::sys_setsid(tf),
        87 => proc::sys_kill(arg0, arg1, tf),
        88 => proc::sys_sigaction(arg0, arg1, arg2, tf),
        89 => proc::sys_sigreturn(tf),
        120 => proc::sys_clone(arg0, arg1, arg2, tf),
        121 => proc::sys_futex(arg0, arg1, arg2, tf),
        122 => proc::sys_gettid(tf),

        // fs.rs — fsync
        130 => fs::sys_fsync(arg0, tf),
        131 => fs::sys_fdatasync(arg0, tf),

        // proc.rs — mmap / munmap / brk / abi-version / unshare
        135 => proc::sys_mmap(arg0, arg1, arg2, arg3, tf),
        136 => proc::sys_munmap(arg0, arg1, tf),
        137 => proc::sys_brk(arg0, tf),
        138 => proc::sys_abi_version(tf),
        139 => proc::sys_unshare(arg0, tf),

        // trace.rs — eBPF-compatible observability
        140 => crate::trace::sys_trace_load(arg0, arg1, tf),
        141 => crate::trace::sys_trace_attach(arg0, arg1, tf),
        142 => crate::trace::sys_trace_read(arg0, arg1, tf),

        // fs.rs
        2 => fs::sys_write(arg0, arg1, arg2, tf),
        5 => fs::sys_read(arg0, arg1, arg2, tf),
        9 => fs::sys_close(arg0, tf),
        55 => fs::sys_chdir(arg0, arg1, tf),
        56 => fs::sys_getcwd(arg0, arg1, tf),
        63 => fs::sys_vfs_register(tf),
        64 => fs::sys_get_vfs_pid(tf),
        65 => fs::sys_initrd_data(arg0, arg1, arg2, tf),
        110 => fs::sys_proc_list(arg0, arg1, tf),
        111 => fs::sys_proc_status(arg0, arg1, arg2, tf),
        112 => fs::sys_blk_register(tf),
        113 => fs::sys_get_blk_pid(tf),

        // ipc.rs
        3 => ipc::sys_pipe(arg0, tf),
        57 => ipc::sys_dup(arg0, tf),
        58 => ipc::sys_dup2(arg0, arg1, tf),
        61 => ipc::sys_mailbox_send(arg0, arg1, arg2, tf),
        62 => ipc::sys_mailbox_receive(arg0, arg1, arg2, tf),
        81 => ipc::sys_fcntl(arg0, arg1, arg2, tf),
        90 => ipc::sys_ipc_send(arg0, arg1, tf),
        91 => ipc::sys_ipc_receive(arg0, arg1, tf),
        92 => ipc::sys_ipc_sendrec(arg0, arg1, tf),
        93 => ipc::sys_datacopy(arg0, arg1, arg2, arg3, tf.regs[14], tf),

        // net.rs
        10 => net::sys_net_send(arg0, arg1, tf),
        11 => net::sys_net_recv(arg0, arg1, tf),
        40 => net::sys_socket(tf),
        41 => net::sys_daemon_next_socket(tf),
        42 => net::sys_daemon_create_conn(arg0, tf),
        43 => net::sys_accept(arg0, tf),
        44 => net::sys_bind(arg0, arg1, arg2, tf),
        45 => net::sys_listen(arg0, arg1, tf),
        46 => net::sys_connect(arg0, arg1, arg2, tf),
        47 => net::sys_sock_send(arg0, arg1, arg2, tf),
        48 => net::sys_sock_recv(arg0, arg1, arg2, tf),
        49 => net::sys_try_recv(arg0, arg1, arg2, tf),

        // user.rs
        23 => user::sys_setuid(arg0, tf),
        24 => user::sys_setgid(arg0, tf),
        30 => user::sys_getuid(tf),
        31 => user::sys_geteuid(tf),
        32 => user::sys_getgid(tf),
        33 => user::sys_getegid(tf),
        34 => user::sys_authenticate(arg0, arg1, arg2, arg3, tf),
        35 => user::sys_can_sudo(arg0, tf),

        // tty.rs
        37 => tty::sys_set_echo(arg0, tf),
        38 => tty::sys_set_raw(arg0, tf),

        _ => {
            crate::println!("Unknown syscall: {}", syscall_num);
            tf.regs[10] = usize::MAX;
        }
    }
}
