sed -i '' -e '/pub const VFS_FD_BASE: u32 = 1000;/,/pub fn close_all_vfs_fds() {/d' /Users/paul/RustroverProjects/openv/user/libos/src/lib.rs
