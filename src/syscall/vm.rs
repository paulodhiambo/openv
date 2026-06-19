use crate::trap::TrapFrame;
use crate::ipc::handle::{KernelObject, Rights};
use crate::mm::vmo::Vmo;
use crate::mm::vmm::{PTE_V, PTE_R, PTE_W, PTE_U};
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use crate::sync::Mutex;

/// Creates a new VMO of `size` bytes.
///
/// Returns a handle to the VMO on success, or an error code on failure.
pub fn sys_vmo_create(size: usize, tf: &mut TrapFrame) {
    if size == 0 {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }
    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };
    let vmo = match Vmo::new(size) {
        Some(v) => v,
        None => { tf.regs[10] = crate::errno::ENOMEM; return; }
    };
    let obj = KernelObject::Vmo(Arc::new(Mutex::new(vmo)));
    let handle = proc.fds.lock().insert_with_rights(obj, Rights::READ | Rights::WRITE | Rights::DUPLICATE | Rights::TRANSFER);
    tf.regs[10] = handle as usize;
}

/// Maps a VMO into the current process's address space.
///
/// # Arguments
/// * `arg0` — VMO handle
/// * `arg1` — virtual address hint (0 = kernel chooses)
/// * `arg2` — mapping size in bytes (clamped to VMO size)
/// * `arg3` — protection flags (bit 0=R, bit 1=W, bit 2=X)
pub fn sys_vmo_map(arg0: usize, arg1: usize, arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let handle = arg0 as u32;
    let va_hint = arg1;
    let map_size = arg2;
    let prot = arg3;

    if map_size == 0 {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    let vmo_arc = {
        let fds = proc.fds.lock();
        match fds.get(handle) {
            Some(KernelObject::Vmo(arc)) => arc.clone(),
            _ => { tf.regs[10] = crate::errno::EBADF; return; }
        }
    };

    let page_size = crate::mm::pmm::PAGE_SIZE;
    let aligned = (map_size + page_size - 1) & !(page_size - 1);

    let va = if va_hint != 0 && va_hint >= 0x1_0000_0000 {
        va_hint
    } else {
        proc.next_mmap_va.fetch_add(aligned, Ordering::Relaxed)
    };

    let satp = proc.satp_val.load(Ordering::Relaxed);
    let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
    let pt = unsafe { &mut *(root_pa as *mut crate::mm::vmm::PageTable) };

    let mut pte_flags = PTE_V | PTE_U;
    if prot & 1 != 0 || prot == 0 { pte_flags |= PTE_R; }
    if prot & 2 != 0 { pte_flags |= PTE_W; }
    if prot & 4 != 0 { pte_flags |= crate::mm::vmm::PTE_X; }

    let vmo = vmo_arc.lock();
    let pages = vmo.pages();
    let num_pages = aligned / page_size;
    let map_pages = num_pages.min(pages.len());

    for i in 0..map_pages {
        let pa = pages[i];
        crate::mm::pmm::incr_ref(pa);
        let page_va = va + i * page_size;
        if pt.map_page(page_va, pa, pte_flags).is_err() {
            crate::mm::pmm::decr_ref_and_maybe_free(pa);
            tf.regs[10] = crate::errno::ENOMEM;
            return;
        }
    }

    unsafe { core::arch::asm!("sfence.vma") };
    tf.regs[10] = va;
}

/// Unmaps a previously mapped VMO region.
///
/// # Arguments
/// * `arg0` — virtual address (page-aligned)
/// * `arg1` — size in bytes
pub fn sys_vmo_unmap(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let va = arg0;
    let size = arg1;
    if size == 0 { tf.regs[10] = 0; return; }

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    let page_size = crate::mm::pmm::PAGE_SIZE;
    let aligned = (size + page_size - 1) & !(page_size - 1);
    let satp = proc.satp_val.load(Ordering::Relaxed);
    let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;

    crate::mm::vmm::unmap_range(root_pa, va, aligned);
    unsafe { core::arch::asm!("sfence.vma") };
    tf.regs[10] = 0;
}

/// Reads bytes from a VMO at a given offset into a user buffer.
///
/// # Arguments
/// * `arg0` — VMO handle
/// * `arg1` — offset in bytes within the VMO
/// * `arg2` — user-space destination pointer
/// * `arg3` — number of bytes to read
pub fn sys_vmo_read(arg0: usize, arg1: usize, arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let handle = arg0 as u32;
    let offset = arg1;
    let buf_ptr = arg2;
    let len = arg3;

    if len == 0 { tf.regs[10] = 0; return; }

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let vmo_arc = {
        let fds = proc.fds.lock();
        match fds.get_with_rights(handle, Rights::READ) {
            Ok(KernelObject::Vmo(arc)) => arc.clone(),
            _ => { tf.regs[10] = crate::errno::EBADF; return; }
        }
    };

    let vmo = vmo_arc.lock();
    let vmo_size = vmo.size();
    if offset >= vmo_size {
        tf.regs[10] = 0;
        return;
    }

    let readable = (vmo_size - offset).min(len);
    let pages = vmo.pages();
    let page_size = crate::mm::pmm::PAGE_SIZE;
    let dest = buf_ptr as *mut u8;

    let mut copied = 0usize;
    while copied < readable {
        let abs_offset = offset + copied;
        let page_idx = abs_offset / page_size;
        let page_off = abs_offset % page_size;
        if page_idx >= pages.len() { break; }
        let chunk = (page_size - page_off).min(readable - copied);
        let src = (pages[page_idx] + page_off) as *const u8;
        unsafe { core::ptr::copy_nonoverlapping(src, dest.add(copied), chunk); }
        copied += chunk;
    }

    tf.regs[10] = copied;
}

/// Writes bytes from a user buffer into a VMO at a given offset.
///
/// # Arguments
/// * `arg0` — VMO handle
/// * `arg1` — offset in bytes within the VMO
/// * `arg2` — user-space source pointer
/// * `arg3` — number of bytes to write
pub fn sys_vmo_write(arg0: usize, arg1: usize, arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let handle = arg0 as u32;
    let offset = arg1;
    let buf_ptr = arg2;
    let len = arg3;

    if len == 0 { tf.regs[10] = 0; return; }

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let vmo_arc = {
        let fds = proc.fds.lock();
        match fds.get_with_rights(handle, Rights::WRITE) {
            Ok(KernelObject::Vmo(arc)) => arc.clone(),
            _ => { tf.regs[10] = crate::errno::EBADF; return; }
        }
    };

    let vmo = vmo_arc.lock();
    let vmo_size = vmo.size();
    if offset >= vmo_size {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }

    let writable = (vmo_size - offset).min(len);
    let pages = vmo.pages();
    let page_size = crate::mm::pmm::PAGE_SIZE;
    let src = buf_ptr as *const u8;

    let mut copied = 0usize;
    while copied < writable {
        let abs_offset = offset + copied;
        let page_idx = abs_offset / page_size;
        let page_off = abs_offset % page_size;
        if page_idx >= pages.len() { break; }
        let chunk = (page_size - page_off).min(writable - copied);
        let dst = (pages[page_idx] + page_off) as *mut u8;
        unsafe { core::ptr::copy_nonoverlapping(src.add(copied), dst, chunk); }
        copied += chunk;
    }

    tf.regs[10] = copied;
}

/// Closes a kernel object handle.
///
/// # Arguments
/// * `arg0` — handle to close
pub fn sys_handle_close(arg0: usize, tf: &mut TrapFrame) {
    let handle = arg0 as u32;
    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };
    match proc.fds.lock().remove(handle) {
        Some(_) => tf.regs[10] = 0,
        None    => tf.regs[10] = crate::errno::EBADF,
    }
}
