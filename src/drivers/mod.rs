use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::sync::Mutex;

pub trait Driver: Send + Sync {
    fn on_interrupt(&self, irq: usize);
}

type ProbeFn = fn(base: usize, irq: usize) -> Option<Box<dyn Driver>>;

struct DriverEntry {
    compatible: &'static [&'static str],
    probe: ProbeFn,
}

static DRIVER_TABLE: &[DriverEntry] = &[
    DriverEntry {
        compatible: &["virtio,mmio"],
        probe: crate::net::virtio_mmio::probe_driver,
    },
];

pub static ACTIVE_DRIVERS: Mutex<Vec<Box<dyn Driver>>> = Mutex::new(Vec::new());

pub fn probe_all(dtb_ptr: usize) {
    if dtb_ptr == 0 {
        return;
    }
    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => return,
    };
    for node in fdt.all_nodes() {
        if let Some(comp_prop) = node.property("compatible") {
            if let Some(comp_str) = comp_prop.as_str() {
                for entry in DRIVER_TABLE {
                    if entry.compatible.iter().any(|c| comp_str.contains(c)) {
                        let mut base = node.property("reg").and_then(|p| p.as_usize()).unwrap_or(0);
                        if base == 0 {
                            if let Some(idx) = node.name.rfind('@') {
                                if let Ok(x) = usize::from_str_radix(&node.name[idx + 1..], 16) {
                                    base = x;
                                }
                            }
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
}

pub fn dispatch_interrupt(irq: usize) {
    for drv in ACTIVE_DRIVERS.lock().iter() {
        drv.on_interrupt(irq);
    }
}
