//! What the process costs: CPU time and memory. A browser page cannot read
//! either, so on the web both are `None`.

use std::time::Duration;

/// CPU time this process has used, on all threads, since it started.
#[cfg(not(target_family = "wasm"))]
pub fn cpu_time() -> Option<Duration> {
    Some(cpu_time::ProcessTime::try_now().ok()?.as_duration())
}

/// The process's resident memory, in bytes.
#[cfg(not(target_family = "wasm"))]
pub fn resident_memory() -> Option<u64> {
    Some(memory_stats::memory_stats()?.physical_mem as u64)
}

/// CPU time this process has used: not readable in a browser.
#[cfg(target_family = "wasm")]
pub fn cpu_time() -> Option<Duration> {
    None
}

/// The process's resident memory: not readable in a browser.
#[cfg(target_family = "wasm")]
pub fn resident_memory() -> Option<u64> {
    None
}
