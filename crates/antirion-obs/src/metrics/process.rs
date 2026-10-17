//! The standard process collector, read at scrape time.

use std::fmt::Write;

use super::expo::{Format, header, number};

pub(super) fn render(out: &mut String, format: Format) {
    let cpu = cpu_time::ProcessTime::try_now()
        .map(|t| t.as_duration().as_secs_f64())
        .ok();
    if let Some(cpu) = cpu {
        header(
            out,
            format,
            "process_cpu_seconds_total",
            "Total user and system CPU time spent in seconds",
            "counter",
        );
        let _ = writeln!(out, "process_cpu_seconds_total {}", number(cpu));
    }
    if let Some(mem) = memory_stats::memory_stats() {
        header(
            out,
            format,
            "process_resident_memory_bytes",
            "Resident memory size in bytes",
            "gauge",
        );
        let _ = writeln!(out, "process_resident_memory_bytes {}", mem.physical_mem);
        header(
            out,
            format,
            "process_virtual_memory_bytes",
            "Virtual memory size in bytes",
            "gauge",
        );
        let _ = writeln!(out, "process_virtual_memory_bytes {}", mem.virtual_mem);
    }
    if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
        header(
            out,
            format,
            "process_open_fds",
            "Number of open file descriptors",
            "gauge",
        );
        let _ = writeln!(out, "process_open_fds {}", entries.count());
    }
}
