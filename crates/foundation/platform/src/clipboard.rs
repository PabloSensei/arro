//! Copying a secret, so that the clipboard history and the cloud clipboard of Windows keep
//! nothing of it. Other systems have no such formats: they answer with
//! [`io::ErrorKind::Unsupported`], and the caller copies the usual way.

use std::io;

/// Puts `text` on the clipboard with the formats that keep it out of the history, the
/// cloud clipboard and the clipboard monitors of Windows.
#[cfg(windows)]
pub fn copy_secret(text: &str) -> io::Result<()> {
    windows::copy_secret(text)
}

/// Other systems keep no clipboard exclusions for a secret.
#[cfg(not(windows))]
pub fn copy_secret(_text: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system has no clipboard formats that keep a secret out of the history",
    ))
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::iter::once;
    use std::thread::sleep;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };

    /// The clipboard format of text, the one that a paste reads.
    const CF_UNICODETEXT: u32 = 13;

    /// The formats that tell the clipboard history, the cloud clipboard and the monitors to
    /// keep the content out. The two `Can...` formats refuse when their DWORD is zero; the
    /// monitor format counts by its presence, and it gets the same zero DWORD.
    const EXCLUSIONS: [&str; 3] = [
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ];

    /// How often the clipboard is asked to open: another program may hold it for a moment.
    const OPEN_TRIES: u32 = 5;
    const OPEN_WAIT: Duration = Duration::from_millis(10);

    pub fn copy_secret(text: &str) -> io::Result<()> {
        let units: Vec<u8> = text
            .encode_utf16()
            .chain(once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        open_clipboard()?;
        let result = fill(&units);
        // SAFETY: this thread opened the clipboard in `open_clipboard`.
        unsafe { CloseClipboard() };
        result
    }

    /// Replaces the content of the open clipboard with the text and the exclusions.
    fn fill(text: &[u8]) -> io::Result<()> {
        // SAFETY: the clipboard is open for this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(io::Error::last_os_error());
        }
        set_data(CF_UNICODETEXT, text)?;
        for name in EXCLUSIONS {
            let format = register(name)?;
            set_data(format, &0_u32.to_le_bytes())?;
        }
        Ok(())
    }

    fn open_clipboard() -> io::Result<()> {
        for attempt in 0..OPEN_TRIES {
            // SAFETY: no window owns the clipboard for the call; a null owner is allowed.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(());
            }
            if attempt + 1 < OPEN_TRIES {
                sleep(OPEN_WAIT);
            }
        }
        Err(io::Error::last_os_error())
    }

    /// The number of a clipboard format of this name, registered for the session.
    fn register(name: &str) -> io::Result<u32> {
        let wide: Vec<u16> = name.encode_utf16().chain(once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        if format == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(format)
    }

    /// Gives the open clipboard a copy of `bytes` in `format`. The clipboard owns the memory
    /// once it accepts it; a refusal frees it here.
    fn set_data(format: u32, bytes: &[u8]) -> io::Result<()> {
        // SAFETY: a movable block is what the clipboard takes; the block is locked only while
        // the bytes are copied into it.
        let memory: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        let target = unsafe { GlobalLock(memory) };
        if target.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: the block was allocated above and nothing else has it.
            unsafe { GlobalFree(memory) };
            return Err(error);
        }
        // SAFETY: the block holds `bytes.len()` bytes, and `bytes` does not overlap it.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast(), bytes.len()) };
        // SAFETY: the block was locked above.
        unsafe { GlobalUnlock(memory) };
        // SAFETY: the clipboard is open for this thread, and `memory` is a movable block.
        if unsafe { SetClipboardData(format, memory) }.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: the clipboard refused the block, so it is still ours to free.
            unsafe { GlobalFree(memory) };
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    use super::copy_secret;

    /// The clipboard format of text, as the Windows headers number it.
    const CF_UNICODETEXT: u32 = 13;

    /// The number the clipboard gives a format of this name.
    fn format_of(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    }

    /// The whole block that the open clipboard holds in `format`, or `None` without it.
    fn held(format: u32) -> Option<Vec<u8>> {
        // SAFETY: the clipboard is open for this thread; the block is locked only while its
        // bytes are copied out.
        unsafe {
            if IsClipboardFormatAvailable(format) == 0 {
                return None;
            }
            let memory = GetClipboardData(format);
            if memory.is_null() {
                return None;
            }
            let source = GlobalLock(memory);
            if source.is_null() {
                return None;
            }
            let size = GlobalSize(memory);
            let bytes = std::slice::from_raw_parts(source.cast::<u8>(), size).to_vec();
            GlobalUnlock(memory);
            Some(bytes)
        }
    }

    /// Writes the clipboard of this machine, so it runs by name and only on purpose. The text
    /// and the three exclusion formats are read back from the clipboard.
    #[test]
    #[ignore = "writes the clipboard of the machine: run it by name"]
    fn a_secret_keeps_its_text_and_the_exclusion_formats() {
        copy_secret("arro probe").unwrap();
        // SAFETY: no window owns the clipboard for the call, and a null owner is allowed.
        assert_ne!(unsafe { OpenClipboard(std::ptr::null_mut()) }, 0);
        let text = held(CF_UNICODETEXT);
        let monitor = held(format_of("ExcludeClipboardContentFromMonitorProcessing"));
        let history = held(format_of("CanIncludeInClipboardHistory"));
        let cloud = held(format_of("CanUploadToCloudClipboard"));
        // SAFETY: this thread opened the clipboard above.
        unsafe { CloseClipboard() };

        let text = text.expect("the text is on the clipboard");
        let units: Vec<u16> = text
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        assert_eq!(String::from_utf16_lossy(&units), "arro probe");
        assert!(
            monitor.is_some(),
            "the monitor exclusion is on the clipboard"
        );
        for (name, value) in [("history", history), ("cloud", cloud)] {
            let value = value.unwrap_or_else(|| panic!("{name} is on the clipboard"));
            assert_eq!(&value[..4], &[0, 0, 0, 0], "{name} refuses the content");
        }
    }
}
