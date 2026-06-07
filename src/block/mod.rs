//! # Block Device Drivers
//!
//! This module is a placeholder for block device drivers. Block devices are
//! storage devices that can be read and written in fixed-size blocks,
//! such as hard drives, SSDs, and flash storage.
//!
//! Currently, OpenV does not include any block device drivers. The
//! `/vfs-server` user-space process handles file system operations using
//! the initrd (initial ramdisk) as its backing store.
//!
//! Future versions of OpenV may include block device drivers for:
//! - VirtIO block devices
//! - AHCI/SATA devices
//! - NVMe devices
//! - SD cards
//!
