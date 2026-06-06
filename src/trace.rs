/// In-kernel trace point system with a minimal eBPF-compatible bytecode VM.
///
/// Programs are loaded via sys_trace_load (140), attached to trace points via
/// sys_trace_attach (141), and their output is read via sys_trace_read (142).
///
/// When a trace point fires it executes all attached programs.  A program that
/// returns R0 == 1 causes the raw TraceCtx to be appended to the ring buffer.
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::Mutex;

// ── Trace point IDs ───────────────────────────────────────────────────────────

pub const TP_SYSCALL_ENTER: u32 = 0;
pub const TP_SYSCALL_EXIT:  u32 = 1;
pub const TP_PROC_CREATE:   u32 = 2;
pub const TP_PROC_EXIT:     u32 = 3;
pub const TP_PAGE_FAULT:    u32 = 4;
const NUM_TP: usize = 5;

// ── Context passed to the VM (R1 points here at program start) ────────────────

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TraceCtx {
    pub tp_id:  u64,
    pub pid:    u64,
    pub ts:     u64,  // CLINT ticks
    pub arg0:   u64,
    pub arg1:   u64,
    pub arg2:   u64,
    pub arg3:   u64,
    pub retval: u64,
}

// ── eBPF instruction encoding (8-byte instructions) ──────────────────────────
//
//  +--------+--------+--------+--------+--------+--------+--------+--------+
//  |   op   |dst|src |   off (i16 LE)  |          imm (i32 LE)             |
//  +--------+--------+--------+--------+--------+--------+--------+--------+
//
// op bit[2:0] = class; the same class+op encoding as Linux eBPF.

const BPF_ALU64_IMM_MOV: u8 = 0xb7;
const BPF_ALU64_IMM_ADD: u8 = 0x07;
const BPF_ALU64_IMM_OR:  u8 = 0x47;
const BPF_ALU64_IMM_AND: u8 = 0x57;
const BPF_ALU64_IMM_XOR: u8 = 0xa7;
const BPF_ALU64_IMM_LSH: u8 = 0x67;
const BPF_ALU64_IMM_RSH: u8 = 0x77;

const BPF_ALU64_REG_MOV: u8 = 0xbf;
const BPF_ALU64_REG_ADD: u8 = 0x0f;
const BPF_ALU64_REG_SUB: u8 = 0x1f;
const BPF_ALU64_REG_AND: u8 = 0x5f;
const BPF_ALU64_REG_OR:  u8 = 0x4f;
const BPF_ALU64_REG_XOR: u8 = 0xaf;

const BPF_LDX_DW:  u8 = 0x79; // R[dst] = *(u64*)(R[src] + off)
const BPF_STX_DW:  u8 = 0x7b; // *(u64*)(R[dst] + off) = R[src]

const BPF_JA:      u8 = 0x05; // PC += off + 1
const BPF_JEQ_IMM: u8 = 0x15;
const BPF_JNE_IMM: u8 = 0x55;
const BPF_JGT_IMM: u8 = 0x25;
const BPF_JGE_IMM: u8 = 0x35;
const BPF_JLT_IMM: u8 = 0xa5;
const BPF_JLE_IMM: u8 = 0xb5;
const BPF_JEQ_REG: u8 = 0x1d;
const BPF_JNE_REG: u8 = 0x5d;
const BPF_JGT_REG: u8 = 0x2d;
const BPF_JGE_REG: u8 = 0x3d;
const BPF_CALL:    u8 = 0x85; // helper call, imm = helper id
const BPF_EXIT:    u8 = 0x95;

const MAX_INSNS:   usize = 512;
const MAX_PROG_ID: usize = 16;
const RING_SIZE:   usize = 8192;

// ── Global state ──────────────────────────────────────��───────────────────────

struct TraceState {
    /// Loaded programs indexed by prog_id.
    programs:    [Option<Arc<Vec<u64>>>; MAX_PROG_ID],
    /// Attached program IDs per trace point.
    attachments: [Vec<usize>; NUM_TP],
    /// Lock-protected ring buffer: (data, write_pos, total_written)
    ring:        ([u8; RING_SIZE], usize, usize),
}

static TRACE: Mutex<TraceState> = Mutex::new(TraceState {
    programs:    [const { None }; MAX_PROG_ID],
    attachments: [const { Vec::new() }; NUM_TP],
    ring:        ([0u8; RING_SIZE], 0, 0),
});

// ── VM interpreter ──────────���─────────────────────────���───────────────────────

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

        macro_rules! dst { () => { r[dr.min(9)] } }
        macro_rules! src { () => { r[sr.min(9)] } }
        macro_rules! imm64 { () => { imm as i64 as u64 } }
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
                    1 => crate::posix::process::current_pid() as u64, // get_pid
                    2 => riscv::register::time::read() as u64,         // get_time
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

// ── Ring buffer helpers ───────────────��───────────────────────────────────────

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

// ── Public trace-point invocation ─────────────���───────────────────────────────

/// Fire all programs attached to `tp_id` with the given context.
/// Called from hot paths (trap handler, syscall entry) — holds the TRACE lock
/// only for the duration of the VM run.
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
            TRACE.lock().ring.0; // borrow check trick — ensure state is accessible
            let mut s = TRACE.lock();
            ring_push(&mut s.ring, ctx);
        }
    }
}

// ── Syscall handlers ──────────────��──────────────────────────���────────────────

/// syscall 140 — load a BPF program from user space.
/// arg0 = prog_ptr (array of u64 instructions), arg1 = prog_len (instruction count)
/// Returns prog_id (0..MAX_PROG_ID) or usize::MAX on error.
pub fn sys_trace_load(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    let n = arg1;
    if n == 0 || n > MAX_INSNS {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }
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

/// syscall 141 — attach program `prog_id` to trace point `tp_id`.
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

/// syscall 142 — copy up to `buf_len` bytes from the ring buffer to `buf_ptr`.
/// Returns bytes copied, or usize::MAX on error.
pub fn sys_trace_read(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    let buf_ptr = arg0 as *mut u8;
    let buf_len = arg1;
    if buf_len == 0 { tf.regs[10] = 0; return; }

    let state = TRACE.lock();
    let (buf, pos, _) = &state.ring;
    let avail = buf_len.min(RING_SIZE);
    let src_start = pos.wrapping_sub(avail) % RING_SIZE;
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
