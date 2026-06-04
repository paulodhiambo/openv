use crate::mm::pmm::{PAGE_SIZE, alloc_page};
use crate::mm::vmm::{PTE_R, PTE_U, PTE_W, PTE_X, PageTable};
#[repr(C)]
struct ElfHeader {
    ident: [u8; 16],
    type_: u16,
    machine: u16,
    version: u32,
    entry: u64,
    phoff: u64,
    shoff: u64,
    flags: u32,
    ehsize: u16,
    phentsize: u16,
    phnum: u16,
    shentsize: u16,
    shnum: u16,
    shstrndx: u16,
}

#[repr(C)]
struct ProgramHeader {
    type_: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Loads an ELF binary from a byte slice into the given page table.
/// Returns the entry point address.
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
                let pa = alloc_page().ok_or("Failed to alloc page for ELF")?;

                let va_offset = if va >= start_va { va - start_va } else { 0 };
                let page_offset = if start_va > va { start_va - va } else { 0 };

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

                page_table.map_page(va, pa, flags)?;
                va += PAGE_SIZE;
            }
        }
    }

    Ok(header.entry as usize)
}
