//! Print what a device scan finds on this machine, right now.
//!
//! Fixtures prove the rules; this proves the fixtures are not fiction. Run it
//! with a stick unplugged, then plugged in but not mounted, then mounted:
//!
//! ```text
//! cargo run -p volumes --bin scan-live
//! ```

use std::path::PathBuf;
use volumes::{BlockDevice, DeviceClass, DeviceScan, UdevProperties, VolumeSet};

/// Prints a live scan of the tables it is pointed at.
struct Probe {
    /// The block device directory to enumerate.
    sys_block: PathBuf,
    /// The udev property database to consult.
    udev: PathBuf,
}

impl Probe {
    /// A probe of the running system.
    fn live() -> Self {
        Self {
            sys_block: PathBuf::from(DeviceClass::SYS_BLOCK),
            udev: PathBuf::from(UdevProperties::DEFAULT_ROOT),
        }
    }

    /// Show every block device and why the scan did or did not offer it.
    ///
    /// A machine with no removable media scans empty, which looks exactly
    /// like a scan that is broken. This listing tells the two apart.
    fn survey(&self) {
        let sys_block = self.sys_block.as_path();
        let udev = self.udev.as_path();
        let Ok(entries) = std::fs::read_dir(sys_block) else {
            println!("no {} on this machine", sys_block.display());
            return;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        println!("every block device the kernel lists:");
        for name in names {
            let Some(device) = BlockDevice::read(sys_block, &name) else {
                println!("  {name:<12} unreadable");
                continue;
            };
            let props = UdevProperties::read_block_in(udev, device.major(), device.minor());
            println!(
                "  {:<12} {:<10} {:>9} MiB  fs={:<8} {}",
                name,
                format!("{:?}", device.class()),
                device.bytes() / (1024 * 1024),
                props.fs_type().unwrap_or("-"),
                if device.is_candidate() {
                    "candidate"
                } else {
                    "skipped"
                },
            );
        }
        println!();
    }

    /// Show every attached removable device and every mounted volume.
    fn run(&self) {
        self.survey();
        let scan = DeviceScan::scan();
        println!("attached removable devices: {}", scan.len());
        for device in scan.devices() {
            let state = device.mount_point().map_or_else(
                || "not mounted".to_owned(),
                |point| format!("at {}", point.display()),
            );
            println!(
                "  {:<10} {:<8} {:>9} MiB  {:<28} [{}]",
                device.name(),
                device.fs_type(),
                device.bytes() / (1024 * 1024),
                state,
                device.display_name(),
            );
        }

        let mounted = VolumeSet::scan();
        println!("mounted volumes: {}", mounted.len());
        for volume in mounted.volumes() {
            println!(
                "  {:<10} {}",
                volume.label(),
                volume.mount_point().display()
            );
        }

        let mountable = scan.mountable();
        println!("offering a Mount action for {} device(s)", mountable.len());
        for device in mountable {
            println!("  {} ({})", device.source(), device.display_name());
        }
    }
}

fn main() {
    Probe::live().run();
}
