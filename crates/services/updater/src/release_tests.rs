//! The archive of each platform: its name, the unpacking of a compressed tar and the
//! executable bits of the files a release replaces.

use super::*;

fn temp(name: &str) -> PathBuf {
    test_support::scratch(&format!("release-{name}"))
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

/// A tar header of the ustar format the release uses.
fn header(name: &str, size: usize, mode: u32, kind: u8) -> [u8; TAR_BLOCK] {
    let mut header = [0u8; TAR_BLOCK];
    let length = name.len().min(header.len());
    header[..length].copy_from_slice(&name.as_bytes()[..length]);
    write_octal(&mut header[100..108], u64::from(mode));
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], size as u64);
    write_octal(&mut header[136..148], 0);
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    for byte in &mut header[148..156] {
        *byte = b' ';
    }
    let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
    let text = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(text.as_bytes());
    header
}

fn write_octal(field: &mut [u8], value: u64) {
    let last = field.len() - 1;
    let text = format!("{:0width$o}", value, width = last);
    field[..last].copy_from_slice(text.as_bytes());
    field[last] = 0;
}

fn write_tar_gz(path: &Path, files: &[(&str, &[u8], u32)]) {
    let file = File::create(path).unwrap();
    let mut writer = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    for (name, bytes, mode) in files {
        writer
            .write_all(&header(name, bytes.len(), *mode, b'0'))
            .unwrap();
        writer.write_all(bytes).unwrap();
        let padding = (TAR_BLOCK - bytes.len() % TAR_BLOCK) % TAR_BLOCK;
        writer.write_all(&vec![0u8; padding]).unwrap();
    }
    writer.write_all(&[0u8; TAR_BLOCK * 2]).unwrap();
    writer.finish().unwrap();
}

#[test]
fn every_platform_has_its_own_archive_name() {
    let version = version("0.2.0");
    for (platform, expected) in [
        (Platform::WindowsX86_64, "arro-0.2.0-windows-x86_64.zip"),
        (Platform::LinuxX86_64, "arro-0.2.0-linux-x86_64.tar.gz"),
        (Platform::MacosAarch64, "arro-0.2.0-macos-aarch64.zip"),
        (Platform::MacosX86_64, "arro-0.2.0-macos-x86_64.zip"),
    ] {
        assert_eq!(
            archive_name_for(&version, Some(platform)),
            Some(expected.to_owned())
        );
    }
}

#[test]
fn the_release_of_the_build_is_the_archive_of_its_platform() {
    assert_eq!(
        archive_name(&version("0.2.0")),
        archive_name_for(&version("0.2.0"), Platform::current())
    );
    assert_eq!(PLATFORM, Platform::current().map_or("", Platform::name));
}

#[test]
fn a_tar_gz_release_with_its_folder_unpacks_into_the_install_folder() {
    let dir = temp("tar-folder");
    let install = dir.join("install");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("arro"), "old program").unwrap();
    let archive = dir.join("arro-0.2.0-linux-x86_64.tar.gz");
    write_tar_gz(
        &archive,
        &[
            ("arro-0.2.0-linux-x86_64/arro", b"new program", 0o755),
            (
                "arro-0.2.0-linux-x86_64/tools/engine/server.jar",
                b"jar",
                0o644,
            ),
        ],
    );

    stage_as(&archive, &install, Some("arro")).unwrap();

    assert_eq!(
        fs::read_to_string(install.join("arro")).unwrap(),
        "new program"
    );
    assert_eq!(
        fs::read_to_string(install.join("tools/engine/server.jar")).unwrap(),
        "jar"
    );
    assert!(!install.join("arro-0.2.0-linux-x86_64").exists());
}

#[cfg(unix)]
#[test]
fn the_replaced_program_is_executable() {
    let dir = temp("executable");
    let install = dir.join("install");
    fs::create_dir_all(&install).unwrap();
    let old = install.join("arro");
    fs::write(&old, "old program").unwrap();
    platform::set_mode(&old, 0o644).unwrap();
    let archive = dir.join("arro-0.2.0-linux-x86_64.tar.gz");
    write_tar_gz(
        &archive,
        &[("arro-0.2.0-linux-x86_64/arro", b"new program", 0o755)],
    );

    stage_as(&archive, &install, Some("arro")).unwrap();

    let mode = platform::mode(&old).unwrap();
    assert_ne!(mode & 0o111, 0, "the program has no execute bits: {mode:o}");
}

#[cfg(unix)]
#[test]
fn a_program_that_comes_without_a_mode_is_made_executable() {
    let dir = temp("executable-no-mode");
    let install = dir.join("install");
    fs::create_dir_all(&install).unwrap();
    let archive = dir.join("arro-0.2.0-linux-x86_64.tar.gz");
    write_tar_gz(
        &archive,
        &[("arro-0.2.0-linux-x86_64/arro", b"new program", 0o644)],
    );

    stage_as(&archive, &install, Some("arro")).unwrap();

    let mode = platform::mode(&install.join("arro")).unwrap();
    assert_ne!(mode & 0o111, 0, "the program has no execute bits: {mode:o}");
}

#[test]
fn a_bundle_archive_replaces_the_whole_application_folder() {
    let dir = temp("bundle");
    let bundle = dir.join("Arro.app");
    fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    fs::write(bundle.join("Contents/MacOS/arro"), "old program").unwrap();
    let archive = dir.join("Arro.app.zip");
    write_zip(
        &archive,
        &[
            ("Arro.app/Contents/MacOS/arro", b"new program"),
            ("Arro.app/Contents/Info.plist", b"plist"),
        ],
    );

    stage_bundle(&archive, &bundle, Some("arro")).unwrap();

    assert_eq!(
        fs::read_to_string(bundle.join("Contents/MacOS/arro")).unwrap(),
        "new program"
    );
    assert_eq!(
        fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap(),
        "plist"
    );
    assert_eq!(
        fs::read_to_string(old_bundle(&bundle).join("Contents/MacOS/arro")).unwrap(),
        "old program"
    );

    assert_eq!(remove_old_bundle(&bundle).unwrap(), 1);
    assert!(!old_bundle(&bundle).exists());
    assert!(bundle.is_dir());
}
