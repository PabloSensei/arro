//! The system version, as the first lines of a panic or crash log name it.

/// `Linux 6.8.0-31-generic (Ubuntu 24.04 LTS)`. The kernel release comes from `/proc`,
/// the distribution name from `/etc/os-release`.
pub fn version() -> String {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_else(|_| "unknown version".to_owned());
    match distribution() {
        Some(name) => format!("{name} {kernel}"),
        None => format!("Linux {kernel}"),
    }
}

fn distribution() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let name = text.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        Some(value.trim().trim_matches('"').to_owned())
    })?;
    (!name.is_empty()).then_some(name)
}
