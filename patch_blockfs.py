import re

with open("src/vfs/blockfs.rs", "r") as f:
    content = f.read()

# 1. Remove kernel imports
content = re.sub(r'use crate::block::VirtioBlk;\n', '', content)
content = re.sub(r'use crate::vfs::\{DirEntry, Stat, VnodeType\};\n', '', content)
content = re.sub(r'use crate::sync::Mutex;\n', 'use spin::Mutex;\n', content)

# 2. Add our imports
new_imports = """
use libos::ipc::Message;

pub struct VirtioBlkProxy {
    pid: i32,
}

impl VirtioBlkProxy {
    pub fn new(pid: i32) -> Self { Self { pid } }

    pub fn read_block(&self, blk: u32, buf: &mut [u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 100;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_mut_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }

    pub fn write_block(&self, blk: u32, buf: &[u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 101;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }
}
"""
content = re.sub(r'// ── On-disk constants', new_imports + '\n// ── On-disk constants', content)

# 3. Replace VirtioBlk with VirtioBlkProxy in OfsState
content = re.sub(r"dev: &'static VirtioBlk", "dev: VirtioBlkProxy", content)

# 4. Remove mkfs and try_mount and Vnode implementations, we'll keep only the raw file/dir helpers.
# Let's just truncate the file from "fn read_magic" to the end.
truncate_idx = content.find('fn read_magic')
if truncate_idx != -1:
    content = content[:truncate_idx]

# 5. Make OfsState and its methods public
content = re.sub(r'impl OfsState \{', 'impl OfsState {', content)
content = content.replace('fn alloc_inode', 'pub fn alloc_inode')
content = content.replace('fn file_read', 'pub fn file_read')
content = content.replace('fn file_write', 'pub fn file_write')
content = content.replace('fn file_truncate', 'pub fn file_truncate')
content = content.replace('fn dir_lookup_inode', 'pub fn dir_lookup_inode')
content = content.replace('fn dir_add_entry', 'pub fn dir_add_entry')
content = content.replace('fn dir_readall', 'pub fn dir_readall')
content = content.replace('fn dir_unlink', 'pub fn dir_unlink')
content = content.replace('fn read_inode', 'pub fn read_inode')
content = content.replace('fn write_inode', 'pub fn write_inode')
content = content.replace('fn free_inode', 'pub fn free_inode')

# 6. Make RawInode public
content = content.replace('struct RawInode', 'pub struct RawInode')

# Add a simple mkfs and init to OfsState
init_code = """
    pub fn new(dev: VirtioBlkProxy) -> Self {
        let mut buf = [0u8; BLOCK_SIZE];
        if !dev.read_block(0, &mut buf) {
            // Error
        }
        let total_blocks = 2048; // For now
        let mut state = Self { dev, total_blocks };
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap_or([0; 4]));
        if magic != OFS_MAGIC {
            state.mkfs();
        }
        state
    }

    pub fn mkfs(&mut self) {
        let mut buf = [0u8; BLOCK_SIZE];
        buf[0..4].copy_from_slice(&OFS_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&OFS_VERSION.to_le_bytes());
        buf[8..12].copy_from_slice(&self.total_blocks.to_le_bytes());
        self.dev.write_block(0, &buf);

        buf.fill(0);
        for i in 0..DATA_START as usize {
            buf[i / 8] |= 1 << (i % 8);
        }
        self.dev.write_block(BLOCK_BITMAP_BLK, &buf);

        buf.fill(0);
        self.dev.write_block(INODE_BITMAP_BLK, &buf);

        for b in INODE_TABLE_START..INODE_TABLE_START + INODE_TABLE_BLOCKS {
            buf.fill(0);
            self.dev.write_block(b, &buf);
        }

        self.bitmap_set(INODE_BITMAP_BLK, 0, true);
        let root_inode = RawInode {
            itype: ITYPE_DIR,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            _reserved: [0u32; 15],
        };
        self.write_inode(ROOT_INODE, &root_inode);
    }
}
"""

content = content.rstrip()
if content.endswith('}'):
    content = content[:-1] + init_code

# Also make constants public
content = content.replace('const ITYPE_FILE_ENTRY: u8 = 1;', 'pub const ITYPE_FILE_ENTRY: u8 = 1;')
content = content.replace('const ITYPE_DIR_ENTRY: u8 = 2;', 'pub const ITYPE_DIR_ENTRY: u8 = 2;')
content = content.replace('const ITYPE_FILE: u8 = 1;', 'pub const ITYPE_FILE: u8 = 1;')
content = content.replace('const ITYPE_DIR: u8 = 2;', 'pub const ITYPE_DIR: u8 = 2;')
content = content.replace('const ROOT_INODE: u32 = 0;', 'pub const ROOT_INODE: u32 = 0;')

with open("user/vfs-server/src/blockfs.rs", "w") as f:
    f.write(content)

