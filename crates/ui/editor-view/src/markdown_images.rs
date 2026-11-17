//! The pictures a markdown preview takes from the internet: one download with a limit,
//! the format of the bytes and the cache of the state folder. Nothing here runs on the
//! window thread.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use ui_kit::gpui::ImageFormat;

/// How long one picture may take before it is dropped.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(10);
/// The largest picture the preview takes, in bytes.
pub(crate) const MAX_BYTES: usize = 10 * 1024 * 1024;
/// The redirects followed, as the other client of the window does.
const REDIRECTS: u32 = 5;

/// Whether the address of a picture names a server rather than a file of the project.
pub(crate) fn is_remote(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The bytes of a remote picture: what the cache under `cache` holds, or the answer of
/// the server, which the next call finds in the cache too.
pub(crate) fn fetch(url: &str, cache: Option<&Path>) -> Result<Vec<u8>, String> {
    let cached = cache.map(|folder| folder.join(cache_name(url)));
    if let Some(path) = &cached
        && let Ok(bytes) = std::fs::read(path)
        && !bytes.is_empty()
        && bytes.len() <= MAX_BYTES
    {
        return Ok(bytes);
    }
    let bytes = download(url)?;
    if let (Some(folder), Some(path)) = (cache, cached)
        && std::fs::create_dir_all(folder).is_ok()
    {
        let _ = std::fs::write(path, &bytes);
    }
    Ok(bytes)
}

/// The name of the file a picture of `url` is kept under in the cache.
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// One request with the time and the size the preview allows.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .timeout_connect(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(REDIRECTS)
        .max_redirects_will_error(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .map_err(|error| format!("the picture did not come: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered {}", status.as_u16()));
    }
    let length = response
        .headers()
        .get("content-length")
        .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        .and_then(|value| value.parse::<u64>().ok());
    if length.is_some_and(|length| length > MAX_BYTES as u64) {
        return Err(format!("the picture is larger than {MAX_BYTES} bytes"));
    }
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if bytes.len() + read > MAX_BYTES {
                    return Err(format!("the picture is larger than {MAX_BYTES} bytes"));
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("the picture did not come: {error}")),
        }
    }
    if bytes.is_empty() {
        return Err("the server sent no picture".to_owned());
    }
    Ok(bytes)
}

/// The format of the bytes, from their first characters and then from the address.
pub(crate) fn format_of(bytes: &[u8], url: &str) -> Option<ImageFormat> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(ImageFormat::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::Webp);
    }
    if bytes.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    if head.contains("<svg") {
        return Some(ImageFormat::Svg);
    }
    let path = url.split(['#', '?']).next().unwrap_or_default();
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_name, format_of, is_remote};
    use ui_kit::gpui::ImageFormat;

    #[test]
    fn an_address_of_a_server_is_remote_and_a_file_is_not() {
        assert!(is_remote("https://example.com/a.png"));
        assert!(is_remote("HTTP://example.com/a.png"));
        assert!(is_remote("  http://example.com/a.png  "));
        assert!(!is_remote("docs/a.png"));
        assert!(!is_remote("file:///C:/work/a.png"));
        assert!(!is_remote(""));
    }

    #[test]
    fn the_format_comes_from_the_bytes_first_and_the_address_after() {
        assert_eq!(
            format_of(
                &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0],
                "a.bin"
            ),
            Some(ImageFormat::Png)
        );
        assert_eq!(format_of(b"GIF89a....", "a.bin"), Some(ImageFormat::Gif));
        assert_eq!(
            format_of(b"RIFF\x00\x00\x00\x00WEBPVP8 ", "a.bin"),
            Some(ImageFormat::Webp)
        );
        assert_eq!(
            format_of(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", "a.bin"),
            Some(ImageFormat::Svg)
        );
        assert_eq!(format_of(b"not a picture", "a.png"), Some(ImageFormat::Png));
        assert_eq!(format_of(b"not a picture", "a.txt"), None);
    }

    #[test]
    fn the_name_of_a_cached_picture_tells_one_address_from_another() {
        assert_ne!(
            cache_name("https://example.com/a.png"),
            cache_name("https://example.com/b.png")
        );
        assert_eq!(
            cache_name("https://example.com/a.png"),
            cache_name("https://example.com/a.png")
        );
    }
}
