//! Hardware snapshot collected once at startup and reported via Register.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use monitor_proto::v1::HardwareInfo;
use sysinfo::{Disk, Disks, System};

/// Take a best-effort hardware snapshot. Missing fields end up empty / zero.
pub fn collect() -> HardwareInfo {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpus = sys.cpus();
    let cpu_model = cpus
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    let cpu_cores = cpus.len() as u32;

    let mem_bytes = sys.total_memory();
    let swap_bytes = sys.total_swap();

    let disks = Disks::new_with_refreshed_list();
    let disk_bytes: u64 = physical_disks(&disks).map(Disk::total_space).sum();

    HardwareInfo {
        cpu_model,
        cpu_cores,
        mem_bytes,
        swap_bytes,
        disk_bytes,
        os: System::name().unwrap_or_default(),
        os_version: System::os_version().unwrap_or_default(),
        kernel: System::kernel_version().unwrap_or_default(),
        arch: std::env::consts::ARCH.to_string(),
        // sysinfo doesn't expose virtualization; M2 may add a platform probe.
        virtualization: String::new(),
        boot_id: System::boot_time().to_string(),
    }
}

/// Short OS identifier reported in RegisterRequest.os.
#[must_use]
pub fn os_id() -> &'static str {
    std::env::consts::OS
}

/// Real, deduped block devices. Skips pseudo filesystems (tmpfs, devtmpfs,
/// overlay, …) and collapses duplicate mount entries that share a backing
/// device — without this a 20 GB cloud VM that exposes /dev/vda{,1,14,15}
/// plus a handful of tmpfs mounts inflates "disk_bytes" to 5× reality.
pub(crate) fn physical_disks(disks: &Disks) -> impl Iterator<Item = &Disk> {
    let mut seen: HashSet<OsString> = HashSet::new();
    disks.list().iter().filter(move |d| {
        if is_pseudo_fs(d.file_system()) {
            return false;
        }
        seen.insert(d.name().to_os_string())
    })
}

fn is_pseudo_fs(fs: &OsStr) -> bool {
    matches!(
        fs.to_str(),
        Some(
            "tmpfs"
                | "devtmpfs"
                | "devpts"
                | "proc"
                | "sysfs"
                | "cgroup"
                | "cgroup2"
                | "overlay"
                | "squashfs"
                | "debugfs"
                | "tracefs"
                | "securityfs"
                | "fusectl"
                | "nsfs"
                | "autofs"
                | "mqueue"
                | "hugetlbfs"
                | "configfs"
                | "pstore"
                | "bpf"
                | "binfmt_misc"
                | "rpc_pipefs"
        )
    )
}
