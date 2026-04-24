//! Hardware snapshot collected once at startup and reported via Register.

use monitor_proto::v1::HardwareInfo;
use sysinfo::System;

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

    // Best-effort disk total: sum of all physical disks.
    let disk_bytes = sysinfo::Disks::new_with_refreshed_list()
        .iter()
        .map(|d| d.total_space())
        .sum();

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
