//! # Kernel Block Device Abstraction
//!
//! Provides a `BlockDevice` trait and a global registry so multiple block
//! drivers can coexist.  The VirtIO block driver below is used primarily by
//! the swap subsystem for disk-backed eviction.  Userspace drivers (the
//! `virtio-blk-driver` process) handle normal file I/O via the VFS server.
//!
//! ## Device naming
//!
//! Devices are registered under names like `"vda"`, `"vdb"`.  The kernel
//! swap module calls `get_device("vda")` to obtain a reference.
//!
//! ## Thread safety
//!
//! The registry mutex is held only while cloning the Arc; all I/O is
//! performed outside the registry lock.

extern crate alloc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::Mutex;

// ---------------------------------------------------------------------------
// BlockDevice trait
// ---------------------------------------------------------------------------

/// A synchronous kernel-side block device.
pub trait BlockDevice: Send + Sync {
    /// Physical block size in bytes (always 512 for VirtIO block).
    fn block_size(&self) -> usize;

    /// Total number of blocks on the device.
    fn block_count(&self) -> u64;

    /// Read `count` contiguous blocks starting at `lba` into `buf`.
    /// `buf.len()` must equal `count * block_size()`.
    fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str>;

    /// Write `count` contiguous blocks starting at `lba` from `buf`.
    fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str>;
}

// ---------------------------------------------------------------------------
// Global device registry
// ---------------------------------------------------------------------------

struct DevEntry {
    name: String,
    dev:  Arc<dyn BlockDevice>,
}

static REGISTRY: Mutex<Vec<DevEntry>> = Mutex::new(Vec::new());

/// Register a block device under `name` (e.g. `"vda"`).
pub fn register(name: &str, dev: Arc<dyn BlockDevice>) {
    REGISTRY.lock().push(DevEntry { name: String::from(name), dev });
    crate::println!("block: registered device '{}'", name);
}

/// Look up a device by name. Returns None if not found.
pub fn get_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    REGISTRY.lock().iter()
        .find(|e| e.name == name)
        .map(|e| e.dev.clone())
}

/// Number of registered devices.
pub fn device_count() -> usize { REGISTRY.lock().len() }

// ---------------------------------------------------------------------------
// Kernel-side VirtIO block driver (polling, device_id = 2)
// ---------------------------------------------------------------------------
//
// This driver is separate from the userspace `virtio-blk-driver` binary.
// It is used only by kernel subsystems (swap) that need direct block I/O
// without going through IPC.

pub mod virtio_blk;
pub use virtio_blk::probe_and_register;
