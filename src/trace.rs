//! # In-Kernel Trace Point System
//!
//! This module provides an in-kernel trace point system with a minimal
//! eBPF-compatible bytecode VM. It allows user-space programs to load
//! eBPF programs, attach them to trace points, and read trace data from
//! a ring buffer.
//!
//! ## Overview
//!
//! OpenV's trace point system is inspired by Linux's eBPF (extended
//! Berkeley Packet Filter) infrastructure. It allows safe, sandboxed
//! programs to execute in kernel context in response to various events
//! (trace points). This is useful for debugging, performance analysis,
//! and security monitoring.
//!
//! ## Usage
//!
//! Programs are loaded via `sys_trace_load` (syscall 140), attached to
//! trace points via `sys_trace_attach` (syscall 141), and their output
//! is read via `sys_trace_read` (syscall 142).
//!
//! When a trace point fires, it executes all attached programs. A program
//! that returns `R0 == 1` causes the raw [`TraceCtx`] to be appended to
//! the ring buffer.
//!
//! ## Trace Points
//!
//! OpenV supports the following trace points:
//!
//! - [`TP_SYSCALL_ENTER`]: Fires when a syscall is entered.
//!  - [`TP_SYSCALL_EXIT`]: Fires when a syscall exits.
//!  - [`TP_PROC_CREATE`]: Fires when a process is created.
//!  - [`TP_PROC_EXIT`]: Fires when a process exits.
//!  - [`TP_PAGE_FAULT`]: Fires when a page fault occurs.
//!
//! ## eBPF VM
//!
//! The eBPF VM is a register-based virtual machine with 10 general-purpose
//! 64-bit registers (`R0..R9`) and a scratch register (`R10`). Programs
//! are limited to 512 instructions and can only read from the context
//! (via `R1`), not write to arbitrary memory. This ensures that programs
//! cannot crash the kernel or access sensitive data.
//!
//! ## Safety
//!
//! The VM performs bounds checking on all memory accesses to prevent
//! programs from reading outside the context. However, the VM itself
//! is not formally verified, so bugs could potentially be exploited.
//! In a production system, additional safety measures (such as
//! formal verification or additional runtime checks) would be needed.

use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::Mutex;

// ── Trace point IDs ───────────────────────────────────────────────────────────

/// Trace point ID for syscall entry.
///
/// Fires when a syscall is entered, before the syscall handler runs.
/// The context contains the syscall number and arguments.
pub const TP_SYSCALL_ENTER: u32 = 0;
/// Trace point ID for syscall exit.
///
/// Fires when a syscall exits, after the syscall handler returns.
/// The context contains the return value.
pub const TP_SYSCALL_EXIT:  u32 = 1;
/// Trace point ID for process creation.
///
/// Fires when a new process is created (e.g., via `fork` or `exec`).
pub const TP_PROC_CREATE:   u32 = 2;
/// Trace point ID for process exit.
///
/// Fires when a process exits.
pub const TP_PROC_EXIT:     u32 = 3;
/// Trace point ID for page fault.
///
/// Fires when a page fault occurs.
pub const TP_PAGE_FAULT:    u32 = 4;
/// Total number of trace points.
const NUM_TP: usize = 5;

// ── Context passed to the VM (R1 points here at program start) ────────────────

/// Context passed to eBPF programs when a trace point fires.
///
/// This structure is passed to eBPF programs via the `R1` register. Programs
/// can read fields from this context using the `BPF_LDX_DW` instruction.
///
/// # Fields
///
/// * `tp_id` - The trace point ID that fired.
/// * `pid` - The PID of the process that triggered the trace point.
/// * `ts` - The timestamp in CLINT ticks.
/// * `arg0..arg3` - Trace point-specific arguments.
/// * `retval` - The return value (for exit trace points).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TraceCtx {
    /// The trace point ID that fired.
    pub tp_id:  u64,
    /// The PID of the process that triggered the trace point.
    pub pid:    u64,
    /// The timestamp in CLINT ticks.
    pub ts:     u64,
    /// First trace point-specific argument.
    pub arg0:   u64,
    /// Second trace point-specific argument.
    pub arg1:   u64,
    /// Third trace point-specific argument.
    pub arg2:   u64,
    /// Fourth trace point-specific argument.
    pub arg3:   u64,
    /// The return value (for exit trace points).
    pub retval: u64,
}

// ── eBPF instruction encoding (8-byte instructions) ──────────────────────────
//
//  +--------+--------+--------+--------+--------+--------+--------+--------+
//  |   op   |dst|src |   off (i16 LE)  |          imm (i32 LE)             |
//  +--------+--------+--------+--------+--------+--------+--------+--------+
//
// op bit[2:0] = class; the same class+op encoding as Linux eBPF.

/// eBPF opcode: `R[dst] = imm` (64-bit).
const BPF_ALU64_IMM_MOV: u8 = 0xb7;
/// eBPF opcode: `R[dst] += imm` (64-bit).
const BPF_ALU64_IMM_ADD: u8 = 0x07;
/// eBPF opcode: `R[dst] |= imm` (64-bit).
const BPF_ALU64_IMM_OR:  u8 = 0x47;
/// eBPF opcode: `R[dst] &= imm` (64-bit).
const BPF_ALU64_IMM_AND: u8 = 0x57;
/// eBPF opcode: `R[dst] ^= imm` (64-bit).
const BPF_ALU64_IMM_XOR: u8 = 0xa7;
/// eBPF opcode: `R[dst] <<= imm` (64-bit).
const BPF_ALU64_IMM_LSH: u8 = 0x67;
/// eBPF opcode: `R[dst] >>= imm` (64-bit).
const BPF_ALU64_IMM_RSH: u8 = 0x77;

/// eBPF opcode: `R[dst] = R[src]` (64-bit).
const BPF_ALU64_REG_MOV: u8 = 0xbf;
/// eBPF opcode: `R[dst] += R[src]` (64-bit).
const BPF_ALU64_REG_ADD: u8 = 0x0f;
/// eBPF opcode: `R[dst] -= R[src]` (64-bit).
const BPF_ALU64_REG_SUB: u8 = 0x1f;
/// eBPF opcode: `R[dst] &= R[src]` (64-bit).
const BPF_ALU64_REG_AND: u8 = 0x5f;
/// eBPF opcode: `R[dst] |= R[src]` (64-bit).
const BPF_ALU64_REG_OR:  u8 = 0x4f;
/// eBPF opcode: `R[dst] ^= R[src]` (64-bit).
const BPF_ALU64_REG_XOR: u8 = 0xaf;

/// eBPF opcode: `R[dst] = *(u64*)(R[src] + off)`.
const BPF_LDX_DW:  u8 = 0x79;
/// eBPF opcode: `*(u64*)(R[dst] + off) = R[src]`.
const BPF_STX_DW:  u8 = 0x7b;

/// eBPF opcode: unconditional jump, `PC += off + 1`.
const BPF_JA:      u8 = 0x05;
/// eBPF opcode: jump if `R[dst] == imm`.
const BPF_JEQ_IMM: u8 = 0x15;
/// eBPF opcode: jump if `R[dst] != imm`.
const BPF_JNE_IMM: u8 = 0x55;
/// eBPF opcode: jump if `R[dst] > imm` (unsigned).
const BPF_JGT_IMM: u8 = 0x25;
/// eBPF opcode: jump if `R[dst] >= imm` (unsigned).
const BPF_JGE_IMM: u8 = 0x35;
/// eBPF opcode: jump if `R[dst] < imm` (unsigned).
const BPF_JLT_IMM: u8 = 0xa5;
/// eBPF opcode: jump if `R[dst] <= imm` (unsigned).
const BPF_JLE_IMM: u8 = 0xb5;
/// eBPF opcode: jump if `R[dst] == R[src]`.
const BPF_JEQ_REG: u8 = 0x1d;
/// eBPF opcode: jump if `R[dst] != R[src]`.
const BPF_JNE_REG: u8 = 0x5d;
/// eBPF opcode: jump if `R[dst] > R[src]` (unsigned).
const BPF_JGT_REG: u8 = 0x2d;
/// eBPF opcode: jump if `R[dst] >= R[src]` (unsigned).
const BPF_JGE_REG: u8 = 0x3d;
/// eBPF opcode: helper function call, `imm` is the helper ID.
const BPF_CALL:    u8 = 0x85;
/// eBPF opcode: exit, return `R[0]`.
const BPF_EXIT:    u8 = 0x95;

/// Maximum number of instructions per eBPF program.
const MAX_INSNS:   usize = 512;
/// Maximum number of loaded eBPF programs.
const MAX_PROG_ID: usize = 16;
/// Size of the trace ring buffer in bytes.
const RING_SIZE:   usize = 8192;

// ── Global state ──────────────────────────────────────────────────────────────

/// Global trace state.
///
/// This struct holds all trace-related state, including loaded programs,
/// attachments, and the ring buffer. It is protected by a [`Mutex`].
///
/// # Fields
///
/// * `programs` - Loaded programs indexed by `prog_id`.
/// * `attachments` - Attached program IDs per trace point.
/// * `ring` - Lock-protected ring buffer: `(data, write_pos, total_written)`.
struct TraceState {
    /// Loaded programs indexed by `prog_id`.
    programs:    [Option<Arc<Vec<u64>>>; MAX_PROG_ID],
    /// Attached program IDs per trace point.
    attachments: [Vec<usize>; NUM_TP],
    /// Lock-protected ring buffer: `(data, write_pos, total_written)`.
    ring:        ([u8; RING_SIZE], usize, usize),
}

/// Global trace state, protected by a [`Mutex`].
static TRACE: Mutex<TraceState> = Mutex::new(TraceState {
    programs:    [const { None }; MAX_PROG_ID],
    attachments: [const { Vec::new() }; NUM_TP],
    ring:        ([0u8; RING_SIZE], 0, 0),
});

// ── VM interpreter ───────────────────────────────────────────────────────────

/// Runs an eBPF program with the given context.
///
/// This function interprets the eBPF bytecode and returns the value of
/// `R0` after the program exits (via `BPF_EXIT`) or runs to completion.
///
/// # Arguments
///
/// * `prog` - The eBPF program to run, as a slice of 64-bit instructions.
/// * `ctx` - The trace context to pass to the program via `R1`.
///
/// # Returns
///
/// The value of `R0` after the program completes.
///
/// # Safety
///
/// The VM performs bounds checking on all memory accesses (via
/// `BPF_LDX_DW`) to prevent programs from reading outside the context.
/// However, the VM is not formally verified, so bugs could potentially
/// be exploited. The VM also limits programs to [`MAX_INSNS`] instructions
/// to prevent infinite loops.
fn vm_run(prog: &[u64], ctx: &TraceCtx) -> u64 {
    let ctx_ptr = ctx as *const TraceCtx as u64;
    let mut r = [0u64; 11]; // R0..R9, scratch R10
    r[1] = ctx_ptr;
    let mut pc: i32 = 0;
    let limit = prog.len().min(MAX_INSNS) as i32;

    while pc >= 0 && pc < limit {
        let insn = prog[pc as usize];
        let op  = (insn & 0xFF) as u8;
        let dr  = ((insn >> 8) & 0xF) as usize;
        let sr  = ((insn >> 12) & 0xF) as usize;
        let off = ((insn >> 16) as i16) as i32;
        let imm = (insn >> 32) as i32;

        /// Returns a mutable reference to the destination register.
        macro_rules! dst { () => { r[dr.min(9)] } }
        /// Returns a mutable reference to the source register.
        macro_rules! src { () => { r[sr.min(9)] } }
        /// Returns the immediate value sign-extended to 64 bits.
        macro_rules! imm64 { () => { imm as i64 as u64 } }
        /// Conditionally jumps by `off + 1` instructions.
        macro_rules! jmp_if { ($cond:expr) => {
            if $cond { pc += off + 1; continue; }
        }}

        match op {
            BPF_ALU64_IMM_MOV => { dst!() = imm64!(); }
            BPF_ALU64_IMM_ADD => { dst!() = dst!().wrapping_add(imm64!()); }
            BPF_ALU64_IMM_OR  => { dst!() |= imm64!(); }
            BPF_ALU64_IMM_AND => { dst!() &= imm64!(); }
            BPF_ALU64_IMM_XOR => { dst!() ^= imm64!(); }
            BPF_ALU64_IMM_LSH => { dst!() = dst!().wrapping_shl(imm as u32); }
            BPF_ALU64_IMM_RSH => { dst!() = dst!().wrapping_shr(imm as u32); }

            BPF_ALU64_REG_MOV => { dst!() = src!(); }
            BPF_ALU64_REG_ADD => { dst!() = dst!().wrapping_add(src!()); }
            BPF_ALU64_REG_SUB => { dst!() = dst!().wrapping_sub(src!()); }
            BPF_ALU64_REG_AND => { dst!() &= src!(); }
            BPF_ALU64_REG_OR  => { dst!() |= src!(); }
            BPF_ALU64_REG_XOR => { dst!() ^= src!(); }

            BPF_LDX_DW => {
                let addr = src!().wrapping_add(off as i64 as u64) as usize;
                // Only allow reads from the context (R1-based).
                let base = ctx_ptr as usize;
                let ctx_size = core::mem::size_of::<TraceCtx>();
                if addr >= base && addr + 8 <= base + ctx_size {
                    // SAFETY: We've verified that `addr + 8` is within the
                    // context, so this read is valid.
                    dst!() = unsafe { core::ptr::read_unaligned(addr as *const u64) };
                }
            }
            BPF_STX_DW => {} // no arbitrary stores allowed

            BPF_JA       => { pc += off + 1; continue; }
            BPF_JEQ_IMM  => { jmp_if!(dst!() == imm64!()); }
            BPF_JNE_IMM  => { jmp_if!(dst!() != imm64!()); }
            BPF_JGT_IMM  => { jmp_if!(dst!() > imm64!()); }
            BPF_JGE_IMM  => { jmp_if!(dst!() >= imm64!()); }
            BPF_JLT_IMM  => { jmp_if!(dst!() < imm64!()); }
            BPF_JLE_IMM  => { jmp_if!(dst!() <= imm64!()); }
            BPF_JEQ_REG  => { jmp_if!(dst!() == src!()); }
            BPF_JNE_REG  => { jmp_if!(dst!() != src!()); }
            BPF_JGT_REG  => { jmp_if!(dst!() > src!()); }
            BPF_JGE_REG  => { jmp_if!(dst!() >= src!()); }

            BPF_CALL => {
                r[0] = match imm {
                    // Helper 1: get current PID
                    1 => crate::posix::process::current_pid() as u64,
                    // Helper 2: get current time
                    2 => riscv::register::time::read() as u64,
                    // Unknown helper: return 0
                    _ => 0,
                };
            }

            BPF_EXIT => return r[0],

            _ => {} // unknown instruction: ignore
        }
        pc += 1;
    }
    r[0]
}

// ── Ring buffer helpers ──────────────────────────────────────────────────────

/// Pushes a trace context record onto the ring buffer.
///
/// The record is prefixed with a 4-byte length header. If the buffer
/// is full, old records are overwritten.
///
/// # Arguments
///
/// * `ring` - A mutable reference to the ring buffer tuple `(data, write_pos, total_written)`.
/// * `ctx` - The trace context to push onto the ring buffer.
fn ring_push(ring: &mut ([u8; RING_SIZE], usize, usize), ctx: &TraceCtx) {
    let record = unsafe {
        core::slice::from_raw_parts(
            ctx as *const TraceCtx as *const u8,
            core::mem::size_of::<TraceCtx>(),
        )
    };
    let len = record.len();
    // Write len header (4 bytes) + record
    let total = 4 + len;
    let (buf, pos, written) = ring;
    let hdr = (len as u32).to_le_bytes();
    for &b in hdr.iter().chain(record.iter()) {
        buf[*pos % RING_SIZE] = b;
        *pos = pos.wrapping_add(1);
    }
    *written = written.wrapping_add(total);
    let _ = total; // suppress unused warning
}

// ── Public trace-point invocation ────────────────────────────────────────────

/// Fires all programs attached to `tp_id` with the given context.
///
/// This function is called from hot paths (trap handler, syscall entry)
/// and holds the [`TRACE`] lock only for the duration of the VM run.
///
/// # Arguments
///
/// * `tp_id` - The trace point ID to fire.
/// * `ctx` - The trace context to pass to the attached programs.
///
/// # Implementation
///
/// The function:
///
/// 1. Locks the global [`TRACE`] state.
/// 2. Collects the IDs and programs attached to the trace point.
/// 3. Releases the lock.
/// 4. Runs each program (without holding the lock).
/// 5. If a program returns `R0 == 1`, re-acquires the lock and pushes
///    the context onto the ring buffer.
pub fn fire(tp_id: u32, ctx: &TraceCtx) {
    let tp = tp_id as usize;
    if tp >= NUM_TP { return; }

    let state = TRACE.lock();
    // Collect prog IDs first to avoid holding the lock during VM execution.
    let prog_ids: Vec<usize> = state.attachments[tp].clone();
    let progs: Vec<Option<Arc<Vec<u64>>>> = prog_ids.iter()
        .map(|&id| state.programs.get(id).and_then(|p| p.clone()))
        .collect();
    drop(state);

    for prog_arc in progs.into_iter().flatten() {
        let ret = vm_run(&prog_arc, ctx);
        if ret == 1 {
            let mut s = TRACE.lock();
            ring_push(&mut s.ring, ctx);
        }
    }
}

// ── Syscall handlers ─────────────────────────────────────────────────────────

/// Syscall 140 — load a BPF program from user space.
///
/// # Arguments
///
/// * `arg0` - `prog_ptr`: pointer to an array of `u64` instructions.
/// * `arg1` - `prog_len`: number of instructions in the program.
/// * `tf` - The trap frame to write the return value to.
///
/// # Returns
///
/// On success, writes the program ID (0..[`MAX_PROG_ID`]) to `tf.regs[10]`.
/// On error, writes one of:
/// - [`crate::errno::EINVAL`]: Invalid program length.
/// - [`crate::errno::ENOMEM`]: No free program slots.
///
/// # Safety
///
/// The caller must ensure that `prog_ptr` points to valid user memory
/// containing `prog_len` instructions.
pub fn sys_trace_load(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    let n = arg1;
    if n == 0 || n > MAX_INSNS {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }
    // SAFETY: The caller has verified that `arg0` points to valid user
    // memory containing `n` instructions.
    let src = unsafe { core::slice::from_raw_parts(arg0 as *const u64, n) };
    let prog = Arc::new(src.to_vec());

    let mut state = TRACE.lock();
    for id in 0..MAX_PROG_ID {
        if state.programs[id].is_none() {
            state.programs[id] = Some(prog);
            tf.regs[10] = id;
            return;
        }
    }
    tf.regs[10] = crate::errno::ENOMEM;
}

/// Syscall 141 — attach program `prog_id` to trace point `tp_id`.
///
/// # Arguments
///
/// * `arg0` - `tp_id`: the trace point ID to attach to.
/// * `arg1` - `prog_id`: the program ID to attach.
/// * `tf` - The trap frame to write the return value to.
///
/// # Returns
///
/// On success, writes 0 to `tf.regs[10]`.
/// On error, writes one of:
/// - [`crate::errno::EINVAL`]: Invalid `tp_id` or `prog_id`.
/// - [`crate::errno::ENOENT`]: Program `prog_id` is not loaded.
pub fn sys_trace_attach(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    let tp_id   = arg0;
    let prog_id = arg1;
    if tp_id >= NUM_TP || prog_id >= MAX_PROG_ID {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }
    let mut state = TRACE.lock();
    if state.programs[prog_id].is_none() {
        tf.regs[10] = crate::errno::ENOENT;
        return;
    }
    let att = &mut state.attachments[tp_id];
    if !att.contains(&prog_id) {
        att.push(prog_id);
    }
    tf.regs[10] = 0;
}

/// Syscall 142 — copy up to `buf_len` bytes from the ring buffer to `buf_ptr`.
///
/// # Arguments
///
/// * `arg0` - `buf_ptr`: pointer to the destination buffer.
/// * `arg1` - `buf_len`: size of the destination buffer.
/// * `tf` - The trap frame to write the return value to.
///
/// # Returns
///
/// On success, writes the number of bytes copied to `tf.regs[10]`.
/// If `buf_len` is 0, writes 0.
///
/// # Safety
///
/// The caller must ensure that `buf_ptr` points to valid user memory
/// of at least `buf_len` bytes.
pub fn sys_trace_read(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    let buf_ptr = arg0 as *mut u8;
    let buf_len = arg1;
    if buf_len == 0 { tf.regs[10] = 0; return; }

    let state = TRACE.lock();
    let (buf, pos, _) = &state.ring;
    let avail = buf_len.min(RING_SIZE);
    let src_start = pos.wrapping_sub(avail) % RING_SIZE;
    // SAFETY: The caller has verified that `buf_ptr` points to valid
    // user memory of at least `avail` bytes.
    let dst = unsafe { core::slice::from_raw_parts_mut(buf_ptr, avail) };
    // Ring wrap-around copy
    if src_start + avail <= RING_SIZE {
        dst.copy_from_slice(&buf[src_start..src_start + avail]);
    } else {
        let first = RING_SIZE - src_start;
        dst[..first].copy_from_slice(&buf[src_start..]);
        dst[first..].copy_from_slice(&buf[..avail - first]);
    }
    tf.regs[10] = avail;
}
