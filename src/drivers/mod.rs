//! # Device Driver Framework
//!
//! This module provides a simple device driver framework for OpenV. It
//! allows the kernel to discover and initialize device drivers based on
//! the device tree blob (DTB) passed by the bootloader.
//!
//! ## Overview
//!
//! OpenV's driver framework is inspired by Linux's device model, but is
//! simplified for a microkernel architecture. Drivers are registered in
//! a static table and probed during kernel initialization. When a device
//! is found that matches a driver's `compatible` string, the driver's
//! probe function is called to initialize the device.
//!
//! ## Driver Trait
//!
//! All drivers implement the [`Driver`] trait, which requires:
//! - `Send + Sync`: The driver must be safe to share across threads.
//! - [`on_interrupt`]: Called when the device generates an interrupt.
//!
//! [`Driver`]: trait.Driver.html
//! [`on_interrupt`]: trait.Driver.html#tymethod.on_interrupt
//!
//! ## Driver Table
//!
//! The [`DRIVER_TABLE`] static array contains all known drivers. Each
//! entry specifies:
//! - `compatible`: A list of DTB `compatible` strings that the driver
//!   supports.
//! - `probe`: A function that attempts to initialize the device and
//!   return a [`Driver`] instance.
//!
//! Currently, only the VirtIO MMIO driver is registered. Future versions
//! may add more drivers.
//!
//! ## Driver Probing
//!
//! The [`probe_all`] function walks the DTB and attempts to initialize
//! each device that matches a registered driver. Successfully initialized
//! drivers are stored in [`ACTIVE_DRIVERS`] and can receive interrupts
//! via [`dispatch_interrupt`].

pub mod gpu;

use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::sync::Mutex;

/// Trait implemented by all device drivers.
///
/// Drivers must be safe to share across threads (`Send + Sync`) and must
/// provide an [`on_interrupt`] method to handle device interrupts.
///
/// [`on_interrupt`]: #tymethod.on_interrupt
pub trait Driver: Send + Sync {
    /// Called when the device generates an interrupt.
    ///
    /// # Arguments
    ///
    /// * `irq` - The IRQ number of the interrupt.
    fn on_interrupt(&self, irq: usize);
}

/// Type alias for driver probe functions.
///
/// A probe function attempts to initialize a device and return a
/// [`Driver`] instance, or `None` if the device could not be initialized.
type ProbeFn = fn(base: usize, irq: usize) -> Option<Box<dyn Driver>>;

/// Internal structure representing a driver entry in the driver table.
struct DriverEntry {
    /// List of DTB `compatible` strings that the driver supports.
    compatible: &'static [&'static str],
    /// Probe function that attempts to initialize the device.
    probe: ProbeFn,
}

/// Static driver table containing all known drivers.
///
/// Currently, only the VirtIO MMIO driver is registered. Each entry
/// specifies the DTB `compatible` strings it supports and a probe
/// function to initialize the device.
static DRIVER_TABLE: &[DriverEntry] = &[
    DriverEntry {
        compatible: &["virtio,mmio"],
        probe: crate::net::virtio_mmio::probe_driver,
    },
];

/// List of all successfully initialized drivers.
///
/// This list is populated by [`probe_all`] and is protected by a
/// [`Mutex`]. Drivers in this list will receive interrupts via
/// [`dispatch_interrupt`].
pub static ACTIVE_DRIVERS: Mutex<Vec<Box<dyn Driver>>> = Mutex::new(Vec::new());

/// Probes the DTB for devices and initializes matching drivers.
///
/// This function walks the DTB and attempts to initialize each device
/// that matches a registered driver. For each match, the driver's
/// probe function is called with the device's base address and IRQ
/// number. Successfully initialized drivers are added to
/// [`ACTIVE_DRIVERS`].
///
/// # Arguments
///
/// * `dtb_ptr` - The physical address of the device tree blob (DTB).
///   If 0, the function returns immediately.
///
/// # Safety
///
/// The caller must ensure that `dtb_ptr` is a valid DTB address.
pub fn probe_all(dtb_ptr: usize) {
    if dtb_ptr == 0 {
        return;
    }
    // SAFETY: The caller has verified that `dtb_ptr` is a valid DTB address.
    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => return,
    };
    for node in fdt.all_nodes() {
        if let Some(comp_prop) = node.property("compatible")
            && let Some(comp_str) = comp_prop.as_str()
        {
            for entry in DRIVER_TABLE {
                if entry.compatible.iter().any(|c| comp_str.contains(c)) {
                    let mut base = node.property("reg").and_then(|p| p.as_usize()).unwrap_or(0);
                    if base == 0
                        && let Some(idx) = node.name.rfind('@')
                        && let Ok(x) = usize::from_str_radix(&node.name[idx + 1..], 16)
                    {
                        base = x;
                    }
                    let irq = node
                        .property("interrupts")
                        .and_then(|p| p.as_usize())
                        .unwrap_or(0);
                    if let Some(drv) = (entry.probe)(base, irq) {
                        ACTIVE_DRIVERS.lock().push(drv);
                    }
                }
            }
        }
    }
}

/// Dispatches an interrupt to all active drivers.
///
/// This function iterates over all drivers in [`ACTIVE_DRIVERS`] and
/// calls their [`on_interrupt`] method with the given IRQ number.
///
/// # Arguments
///
/// * `irq` - The IRQ number of the interrupt.
///
/// [`ACTIVE_DRIVERS`]: static.ACTIVE_DRIVERS.html
/// [`on_interrupt`]: trait.Driver.html#tymethod.on_interrupt
pub fn dispatch_interrupt(irq: usize) {
    for drv in ACTIVE_DRIVERS.lock().iter() {
        drv.on_interrupt(irq);
    }
}
