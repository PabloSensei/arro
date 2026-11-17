//! The built `arro.exe`: the icon GPUI loads for the window and the strings that make the
//! file properties and the task manager call the program `Arro`. The test reads the
//! resources of the executable as data, it never starts it.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW, LoadResource, LockResource,
    SizeofResource,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON, RT_VERSION};

/// The executable under test, built first when an xtask-only run has not made it yet.
fn binary() -> PathBuf {
    let root = super::workspace_root();
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || root.join("target"),
        |dir| {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                root.join(dir)
            }
        },
    );
    let release = !cfg!(debug_assertions);
    let exe = target
        .join(if release { "release" } else { "debug" })
        .join("arro.exe");
    if !exe.is_file() {
        let mut command = std::process::Command::new(super::cargo());
        command
            .current_dir(&root)
            .args(["build", "-p", "ide", "--bin", "arro"]);
        if release {
            command.arg("--release");
        }
        let status = command.status().expect("cargo build -p ide starts");
        assert!(status.success(), "cargo build -p ide failed");
    }
    assert!(
        exe.is_file(),
        "{} is missing after the build",
        exe.display()
    );
    exe
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The resources of a file loaded as data, so no code of it runs.
struct Module(*mut c_void);

impl Module {
    fn load(file: &Path) -> Self {
        let path = wide(&file.display().to_string());
        let module = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_AS_DATAFILE,
            )
        };
        assert!(!module.is_null(), "cannot load {}", file.display());
        Self(module)
    }

    fn find(&self, name: *const u16, kind: *const u16) -> bool {
        !unsafe { FindResourceW(self.0, name, kind) }.is_null()
    }

    /// The bytes of a resource, copied out of the module before it is freed.
    fn bytes(&self, name: *const u16, kind: *const u16) -> Vec<u8> {
        let found = unsafe { FindResourceW(self.0, name, kind) };
        assert!(
            !found.is_null(),
            "resource {name:?} of kind {kind:?} is missing"
        );
        unsafe {
            let size = SizeofResource(self.0, found);
            let loaded = LoadResource(self.0, found);
            assert!(!loaded.is_null(), "cannot load the resource");
            let data = LockResource(loaded) as *const u8;
            std::slice::from_raw_parts(data, size as usize).to_vec()
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn the_binary_has_icon_one_and_the_product_name_arro() {
    let exe = binary();
    let module = Module::load(&exe);
    // `load_icon` of GPUI asks for the group icon named 1, the frames carry the same id.
    let name: *const u16 = std::ptr::without_provenance(1);
    assert!(
        module.find(name, RT_GROUP_ICON),
        "no icon group 1 in {}",
        exe.display()
    );
    assert!(
        module.find(name, RT_ICON),
        "no icon frame 1 in {}",
        exe.display()
    );

    let version = module.bytes(name, RT_VERSION);
    let key = utf16_bytes("ProductName");
    let index = version
        .windows(key.len())
        .position(|window| window == key)
        .expect("the version resource has no ProductName");
    let value = utf16_bytes("Arro");
    let found = version[index + key.len()..]
        .windows(value.len())
        .take(64)
        .any(|window| window == value);
    assert!(found, "ProductName of {} is not Arro", exe.display());
}
