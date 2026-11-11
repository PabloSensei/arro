//! The Windows version, as the first lines of a panic or crash log name it.

use std::mem::size_of;

use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

/// `Windows 11 10.0.26100`. `RtlGetVersion` reports the real version, while
/// `GetVersionExW` says 6.2 unless the executable carries a compatibility manifest.
pub fn version() -> String {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: the structure is initialized with its own size and is writable.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    if status != 0 {
        return "Windows (unknown version)".to_owned();
    }
    let name = match (info.dwMajorVersion, info.dwBuildNumber) {
        (major, build) if major >= 10 && build >= 22_000 => "Windows 11",
        (major, _) if major >= 10 => "Windows 10",
        (6, 3) => "Windows 8.1",
        (6, 2) => "Windows 8",
        (6, 1) => "Windows 7",
        (6, 0) => "Windows Vista",
        (5, 1) => "Windows XP",
        _ => "Windows",
    };
    format!(
        "{name} {}.{}.{}",
        info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
    )
}
