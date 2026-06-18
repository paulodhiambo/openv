//! # Syscall Dispatch
//!
//! This module is the entry point for all system calls. When a user-space
//! process executes `ecall`, the trap handler ([`crate::trap::rust_trap_handler`])
//! reads the syscall number from `a7` and the first four arguments from
//! `a0`–`a3`, then calls [`dispatch`].
//!
//! ## Syscall Convention
//!
//! - Syscall number in `a7`
//! - Arguments in `a0`, `a1`, `a2`, `a3`, and `a4`
//! - Return value placed in `a0` (trap frame `regs[10]`)
//! - `sepc` has been advanced past the `ecall` instruction by the trap
//!   handler (so the process resumes after the ecall).
//!
//! ## Syscall Categories
//!
//! | Module   | Range  | Description                       |
//! |----------|--------|-----------------------------------|
//! | `pm`     | 100–109| Process/VM management primitives  |
//! | `proc`   | 0–1, 50–89, 120–122 | POSIX process/signal control |
//! | `fs`     | 2, 5, 9, 55–56, 63–65, 110–113, 130–131 | File & FS operations |
//! | `ipc`    | 3, 57–58, 61–62, 81, 90–93 | IPC & fd ops |
//! | `net`    | 10–11, 40–49 | Networking & sockets |
//! | `user`   | 23–24, 30–35 | User/credential ops |
//! | `tty`    | 37–38 | TTY mode control |
//! | `trace`  | 140–142 | eBPF-compatible tracepoints |
//! | `epoll`  | 233, 244, 291 | I/O event notification |
//!
//! ## Error Convention
//!
//! All syscalls return a negative error code on failure. The negative
//! value is `-errno` (e.g., `-1` for `EPERM`, `-2` for `ENOENT`, etc.).
//! See [`crate::errno`].

use crate::trap::TrapFrame;

/// Macro to fetch the current process, or return `ESRCH` to user-space
/// if no current process is found.
///
/// On `Err` branch, this macro sets `a0 = ESRCH` and advances `sepc`
/// past the `ecall` and returns from the enclosing function. This is
/// a shorthand for the common pattern at the top of syscalls that
/// manipulate the current process.
#[macro_export]
macro_rules! get_current_proc_or_esrch {
    ($tf:expr) => {
        match $crate::posix::process::get_current_proc() {
            Some(p) => p,
            None => {
                $tf.regs[10] = $crate::errno::ESRCH;
                $tf.sepc += 4;
                return;
            }
        }
    };
}

pub mod epoll;
pub mod fs;
pub mod ipc;
pub mod net;
pub mod proc;
pub mod tty;
pub mod user;
pub mod pm;
pub mod vm;

/// Dispatches a system call to the appropriate handler.
///
/// This function is called by [`crate::trap::rust_trap_handler`] with
/// the syscall number and arguments extracted from the trap frame. It
/// matches the syscall number to a handler and invokes it. Unknown
/// syscall numbers return `usize::MAX` in `a0` (and print a message
/// to the kernel log).
///
/// # Arguments
///
/// * `syscall_num` - Syscall number (from `a7`).
/// * `arg0`–`arg3` - First four arguments (from `a0`–`a3`).
/// * `tf` - The trap frame of the calling process. Handlers write the
///   return value to `tf.regs[10]`.
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
        74 => proc::sys_job_create(arg0, arg1, tf),
        75 => proc::sys_job_set_policy(arg0, arg1, arg2, tf),
        120 => proc::sys_clone(arg0, arg1, arg2, arg3, tf),
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

        // vm.rs — VMO / handle-close
        150 => vm::sys_vmo_create(arg0, tf),
        151 => vm::sys_vmo_map(arg0, arg1, arg2, arg3, tf),
        152 => vm::sys_vmo_unmap(arg0, arg1, tf),
        153 => vm::sys_vmo_read(arg0, arg1, arg2, arg3, tf),
        154 => vm::sys_vmo_write(arg0, arg1, arg2, arg3, tf),
        155 => vm::sys_handle_close(arg0, tf),

        // proc.rs — thread lifecycle
        158 => proc::sys_exit_group(arg0, tf),
        159 => proc::sys_set_tid_address(arg0, tf),
        160 => proc::sys_tgkill(arg0, arg1, arg2, tf),

        // ipc.rs — object signal wait (Zircon-compatible)
        161 => ipc::sys_object_wait_one(arg0, arg1, arg2, arg3, tf),

        // proc.rs — task kill
        162 => proc::sys_task_kill(arg0, tf),

        // fs.rs — component manager registration
        163 => fs::sys_cm_register(tf),
        164 => fs::sys_get_cm_pid(tf),

        // pm.rs — driver framework isolation
        165 => pm::sys_create_resource(arg0, arg1, arg2, tf),
        166 => pm::sys_grant_driver_cap(arg0, tf),

        // trace.rs — eBPF-compatible observability
        140 => crate::trace::sys_trace_load(arg0, arg1, tf),
        141 => crate::trace::sys_trace_attach(arg0, arg1, tf),
        142 => crate::trace::sys_trace_read(arg0, arg1, tf),

        // fs.rs
        2 => fs::sys_write(arg0, arg1, arg2, tf),
        5 => fs::sys_read(arg0, arg1, arg2, tf),
        8 => fs::sys_open(arg0, arg1, arg2, tf),
        9 => fs::sys_close(arg0, tf),
        55 => fs::sys_chdir(arg0, arg1, tf),
        56 => fs::sys_getcwd(arg0, arg1, tf),
        63 => fs::sys_vfs_register(tf),
        64 => fs::sys_get_vfs_pid(tf),
        65 => fs::sys_initrd_data(arg0, arg1, arg2, tf),
        66 => fs::sys_pm_register(tf),
        67 => fs::sys_get_pm_pid(tf),
        110 => fs::sys_proc_list(arg0, arg1, tf),
        111 => fs::sys_proc_status(arg0, arg1, arg2, tf),
        112 => fs::sys_blk_register(tf),
        113 => fs::sys_get_blk_pid(tf),
        114 => fs::sys_proc_server_register(tf),
        115 => fs::sys_get_proc_server_pid(tf),
        116 => fs::sys_dev_server_register(tf),
        117 => fs::sys_get_dev_server_pid(tf),

        // ipc.rs
        3 => ipc::sys_pipe(arg0, tf),
        57 => ipc::sys_dup(arg0, tf),
        58 => ipc::sys_dup2(arg0, arg1, tf),
        61 => ipc::sys_mailbox_send(arg0, arg1, arg2, tf),
        62 => ipc::sys_mailbox_receive(arg0, arg1, arg2, tf),
        81 => ipc::sys_fcntl(arg0, arg1, arg2, tf),
        70 => ipc::sys_port_create(arg0, tf),
        71 => ipc::sys_port_wait(arg0, arg1, tf),
        72 => ipc::sys_port_queue(arg0, arg1, tf),
        73 => ipc::sys_port_bind(arg0, arg1, arg2 as u64, arg3 as u32, tf),
         76 => ipc::sys_handle_duplicate(arg0, arg1, tf),
         77 => ipc::sys_handle_get_rights(arg0, tf),
        90 => ipc::sys_ipc_send(arg0, arg1, tf),
        91 => ipc::sys_ipc_receive(arg0, arg1, tf),
        92 => ipc::sys_ipc_sendrec(arg0, arg1, tf),
        93 => ipc::sys_datacopy(arg0, arg1, arg2, arg3, tf.regs[14], tf),
        94 => ipc::sys_channel_create(arg0, tf),
        95 => ipc::sys_channel_write(arg0, arg1, arg2, arg3, tf.regs[14], tf),
        96 => ipc::sys_channel_read(arg0, arg1, arg2, arg3, tf.regs[14], tf),

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

        // epoll.rs
        233 => epoll::sys_epoll_ctl(arg0, arg1, arg2, arg3, tf),
        244 => epoll::sys_epoll_pwait(arg0, arg1, arg2, arg3, tf),
        291 => epoll::sys_epoll_create1(arg0, tf),

        _ => {
            crate::println!("Unknown syscall: {}", syscall_num);
            tf.regs[10] = usize::MAX;
        }
    }
}
