//! # ELF Binary Loader
//!
//! This module provides an ELF (Executable and Linkable Format) binary
//! loader. ELF is the standard executable format on Unix-like systems,
//! including Linux and OpenV.
//!
//! ## Overview
//!
//! The loader supports the 64-bit RISC-V ELF format with the following
//! features:
//!
//! - **PT_LOAD segments**: Loadable program segments are mapped into
//!    the process's page table.
//!  - **Read/Write/Execute flags**: Segment permissions are translated
//!    to PTE flags.
//!  - **Bounds checking**: All accesses to the binary data are bounds-checked
//!    to prevent out-of-bounds reads.
//!
//! ## Limitations
//!
//! - Only `PT_LOAD` segments are processed.
//! - Dynamic linking is not supported (no PT_INTERP, PT_DYNAMIC).
//! - No support for section headers or relocations.

use crate::mm::pmm::{PAGE_SIZE, alloc_frame};
use crate::mm::vmm::{PTE_R, PTE_U, PTE_W, PTE_X, PageTable};

/// ELF file header (64-bit).
#[repr(C)]
struct ElfHeader {
    /// ELF identification (magic number, class, data encoding, etc.).
    ident: [u8; 16],
    /// Object file type (2 = executable, 3 = shared object).
    type_: u16,
    /// Target architecture (243 = RISC-V).
    machine: u16,
    /// ELF version.
    version: u32,
    /// Virtual address of the entry point.
    entry: u64,
    /// Offset of the program header table.
    phoff: u64,
    /// Offset of the section header table.
    shoff: u64,
    /// Processor-specific flags.
    flags: u32,
    /// Size of the ELF header.
    ehsize: u16,
    /// Size of a program header entry.
    phentsize: u16,
    /// Number of program header entries.
    phnum: u16,
    /// Size of a section header entry.
    shentsize: u16,
    /// Number of section header entries.
    shnum: u16,
    /// Index of the section header string table.
    shstrndx: u16,
}

/// ELF program header (64-bit).
#[repr(C)]
struct ProgramHeader {
    /// Segment type (1 = PT_LOAD).
    type_: u32,
    /// Segment flags (PF_X, PF_W, PF_R).
    flags: u32,
    /// Offset of the segment in the file.
    offset: u64,
    /// Virtual address of the segment in memory.
    vaddr: u64,
    /// Physical address (unused on most systems).
    paddr: u64,
    /// Size of the segment in the file.
    filesz: u64,
    /// Size of the segment in memory.
    memsz: u64,
    /// Alignment requirement.
    align: u64,
}

/// Program header type: Loadable segment.
const PT_LOAD: u32 = 1;
/// Segment flag: Executable.
const PF_X: u32 = 1;
/// Segment flag: Writable.
const PF_W: u32 = 2;
/// Segment flag: Readable.
const PF_R: u32 = 4;

/// Loads an ELF binary from a byte slice into the given page table.
///
/// This function parses the ELF header and program headers, allocates
/// physical pages for each `PT_LOAD` segment, copies the file data
/// into those pages, and maps them into the page table.
///
/// # Arguments
///
/// * `data` - The ELF binary data.
/// * `page_table` - The page table to load the binary into.
///
/// # Returns
///
/// `Ok(entry_point)` with the virtual address of the entry point on
/// success, or an error if:
/// - The data is too small to be an ELF file.
/// - The ELF magic number is wrong.
/// - The program header table is malformed.
/// - A segment extends past the end of the file.
/// - Memory allocation fails.
///
/// # Safety
///
/// The caller must ensure that `data` points to valid memory containing
/// a valid ELF binary.
pub fn load_elf(data: &[u8], page_table: &mut PageTable) -> Result<usize, &'static str> {
    if data.len() < core::mem::size_of::<ElfHeader>() {
        return Err("File too small");
    }

    let header = unsafe { &*(data.as_ptr() as *const ElfHeader) };
    if header.ident[0..4] != [0x7f, b'E', b'L', b'F'] {
        return Err("Not an ELF file");
    }

    let phoff = header.phoff as usize;
    let phnum = header.phnum as usize;
    let phentsize = header.phentsize as usize;

    // Validate program header table bounds.
    if phentsize < core::mem::size_of::<ProgramHeader>() {
        return Err("invalid program header entry size");
    }
    let ph_end = phoff
        .checked_add(phnum.checked_mul(phentsize).ok_or("integer overflow")?)
        .ok_or("integer overflow")?;
    if ph_end > data.len() {
        return Err("program headers extend past end of file");
    }

    for i in 0..phnum {
        let ph_ptr = data.as_ptr() as usize + phoff + i * phentsize;
        let ph = unsafe { &*(ph_ptr as *const ProgramHeader) };

        if ph.type_ == PT_LOAD {
            let start_va = ph.vaddr as usize;
            let memsz = ph.memsz as usize;
            let end_va = start_va.checked_add(memsz).ok_or("integer overflow")?;
            let offset_in_file = ph.offset as usize;
            let file_size = ph.filesz as usize;

            // Validate that the file data for this segment fits within the binary.
            if file_size > 0 {
                let file_end = offset_in_file
                    .checked_add(file_size)
                    .ok_or("integer overflow")?;
                if file_end > data.len() {
                    return Err("segment data extends past end of file");
                }
            }

            // Reject zero-sized segments.
            if memsz == 0 {
                continue;
            }

            // Flags
            let mut flags = PTE_U;
            if (ph.flags & PF_R) != 0 {
                flags |= PTE_R;
            }
            if (ph.flags & PF_W) != 0 {
                flags |= PTE_W;
            }
            if (ph.flags & PF_X) != 0 {
                flags |= PTE_X;
            }

            let mut va = start_va & !(PAGE_SIZE - 1);

            while va < end_va {
                // Use alloc_frame (RAII) so the page is automatically freed if
                // map_page returns an error — alloc_page().ok_or()? would leak it.
                let frame = alloc_frame().ok_or("Failed to alloc page for ELF")?;
                let pa = frame.pa();

                let va_offset = va.saturating_sub(start_va);
                let page_offset = start_va.saturating_sub(va);

                if va_offset < file_size {
                    let copy_len = core::cmp::min(PAGE_SIZE - page_offset, file_size - va_offset);
                    let file_start = offset_in_file + va_offset;

                    // Bounds-checked by the validation above; still guard against panic.
                    if file_start + copy_len <= data.len() {
                        unsafe {
                            let pa_ptr = (pa + page_offset) as *mut u8;
                            core::ptr::copy_nonoverlapping(
                                data.as_ptr().add(file_start),
                                pa_ptr,
                                copy_len,
                            );
                        }
                    }
                }

                // map_page success: transfer ownership of the frame to the PTE.
                // On error: `frame` is dropped here, freeing the physical page.
                page_table.map_page(va, pa, flags)?;
                frame.into_raw();
                va += PAGE_SIZE;
            }
        }
    }

    Ok(header.entry as usize)
}
