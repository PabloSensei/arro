//! The system version, as the first lines of a panic or crash log name it.

/// `Linux 6.8.0-31-generic (Ubuntu 24.04 LTS)`. The kernel release comes from `/proc`,
/// the distribution name from `/etc/os-release`.
#[cfg(target_os = "linux")]
pub fn version() -> String {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_else(|_| "unknown version".to_owned());
    match distribution() {
        Some(name) => format!("{name} {kernel}"),
        None => format!("Linux {kernel}"),
    }
}

/// `macOS 14.6.1 (23G93)`. The product version and the build come from `sysctl`, which
/// is what `sw_vers` reads too.
#[cfg(target_os = "macos")]
pub fn version() -> String {
    let product = sysctl_text(c"kern.osproductversion");
    let build = sysctl_text(c"kern.osversion");
    match (product, build) {
        (Some(product), Some(build)) => format!("macOS {product} ({build})"),
        (Some(product), None) => format!("macOS {product}"),
        _ => "macOS unknown version".to_owned(),
    }
}

/// One `sysctl` value that is a string.
#[cfg(target_os = "macos")]
fn sysctl_text(name: &std::ffi::CStr) -> Option<String> {
    let mut length = 0usize;
    // SAFETY: a null buffer asks for the length of the value only.
    let sized = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || length == 0 {
        return None;
    }
    let mut value = vec![0u8; length];
    // SAFETY: the buffer holds as many bytes as the call was just told the value takes.
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    value.truncate(length);
    let text = String::from_utf8_lossy(&value);
    let text = text.trim_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(target_os = "linux")]
fn distribution() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let name = text.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        Some(value.trim().trim_matches('"').to_owned())
    })?;
    (!name.is_empty()).then_some(name)
}
