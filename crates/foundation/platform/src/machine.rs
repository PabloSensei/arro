//! Whether the system runs in a virtual machine. A host that gives its guest memory on
//! demand is why the guest sees little free memory for a moment, so the memory watcher
//! needs to know. The firmware names the machine, and Windows keeps its strings in the
//! registry. The CPUID hypervisor bit is no answer: Hyper-V and WSL set it on ordinary
//! computers too.

use std::sync::OnceLock;

/// Words of a vendor or a product that name a hypervisor, in lower case.
const NAMED_WORDS: &[&str] = &["vmware", "virtualbox", "qemu", "kvm", "xen", "parallels"];

/// Phrases, in lower case, that name the clouds which run their guests on a hypervisor.
const NAMED_PHRASES: &[&str] = &["amazon ec2", "google compute engine"];

/// Whether the firmware strings name a virtual machine: the manufacturer and the product
/// of the BIOS, as Windows keeps them. Pure, so the parsing needs no registry.
pub fn bios_names_a_virtual_machine(manufacturer: &str, product: &str) -> bool {
    let manufacturer = manufacturer.to_ascii_lowercase();
    let product = product.to_ascii_lowercase();
    // Hyper-V names its guests "Virtual Machine" under the vendor Microsoft, and a real
    // computer of that vendor has a product name of its own.
    if manufacturer.contains("microsoft") && product.contains("virtual machine") {
        return true;
    }
    let text = format!("{manufacturer} {product}");
    let words: Vec<&str> = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .collect();
    NAMED_WORDS.iter().any(|word| words.contains(word))
        || NAMED_PHRASES.iter().any(|phrase| text.contains(phrase))
}

/// Whether the system runs in a virtual machine. Windows answers from the firmware in the
/// registry, read once; any other system reports none here.
pub fn is_virtual_machine() -> bool {
    static VIRTUAL: OnceLock<bool> = OnceLock::new();
    *VIRTUAL.get_or_init(|| {
        let (manufacturer, product) = firmware_names();
        bios_names_a_virtual_machine(&manufacturer, &product)
    })
}

/// The vendor and the product of the firmware, as the BIOS key of the registry keeps them.
#[cfg(windows)]
fn firmware_names() -> (String, String) {
    (
        bios_value("SystemManufacturer"),
        bios_value("SystemProductName"),
    )
}

/// One string value of the BIOS key, or an empty text when Windows does not give it.
#[cfg(windows)]
fn bios_value(name: &str) -> String {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };

    let key = wide(r"HARDWARE\DESCRIPTION\System\BIOS");
    let value = wide(name);
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: both names are NUL-terminated and live through the call; the handle is
    // written only when the key opens.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut handle) } != 0 {
        return String::new();
    }
    let mut kind = 0u32;
    let mut bytes = 0u32;
    // SAFETY: without a buffer the call only reports how many bytes the value takes.
    let sized = unsafe {
        RegQueryValueExW(
            handle,
            value.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    let mut text = String::new();
    if sized == 0 && kind == REG_SZ && bytes > 0 {
        let mut units = vec![0u16; (bytes as usize).div_ceil(2)];
        // SAFETY: the buffer holds as many bytes as the size the call reported.
        let read = unsafe {
            RegQueryValueExW(
                handle,
                value.as_ptr(),
                std::ptr::null(),
                &mut kind,
                units.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if read == 0 {
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units.len());
            text = String::from_utf16_lossy(&units[..end]);
        }
    }
    // SAFETY: the key was opened above and is closed once, here.
    unsafe { RegCloseKey(handle) };
    text
}

/// A NUL-terminated UTF-16 copy of `text`, for the wide-character calls.
#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Systems other than Windows keep no firmware strings in the registry, so none names a
/// virtual machine here.
#[cfg(not(windows))]
fn firmware_names() -> (String, String) {
    (String::new(), String::new())
}

#[cfg(test)]
mod tests {
    use super::bios_names_a_virtual_machine;

    #[test]
    fn the_firmware_of_a_virtual_machine_is_named() {
        for (manufacturer, product) in [
            ("Microsoft Corporation", "Virtual Machine"),
            ("VMware, Inc.", "VMware7,1"),
            ("innotek GmbH", "VirtualBox"),
            ("QEMU", "Standard PC (i440FX + PIIX, 1996)"),
            ("Xen", "HVM domU"),
            (
                "Parallels Software International Inc.",
                "Parallels Virtual Platform",
            ),
            ("Amazon EC2", "m5.large"),
            ("Google", "Google Compute Engine"),
            ("Red Hat", "KVM"),
        ] {
            assert!(
                bios_names_a_virtual_machine(manufacturer, product),
                "{manufacturer} / {product} is a virtual machine"
            );
        }
    }

    #[test]
    fn the_firmware_of_an_ordinary_computer_is_not_named() {
        for (manufacturer, product) in [
            ("Dell Inc.", "XPS 13 9310"),
            ("Microsoft Corporation", "Surface Laptop 4"),
            ("Gigabyte Technology Co., Ltd.", "B550 AORUS PRO"),
            ("Xenon Systems", "Workstation"),
            ("", ""),
        ] {
            assert!(
                !bios_names_a_virtual_machine(manufacturer, product),
                "{manufacturer} / {product} is an ordinary computer"
            );
        }
    }

    #[test]
    fn a_name_in_the_vendor_or_in_the_product_names_the_machine_alike() {
        assert!(bios_names_a_virtual_machine("QEMU", ""));
        assert!(bios_names_a_virtual_machine("", "VMware Virtual Platform"));
    }
}
