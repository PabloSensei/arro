//! `Content-Length` framing of the base protocol.

use std::io::{Error, ErrorKind, Result};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// A larger frame means a broken stream, not a big document.
const MAX_FRAME: usize = 1 << 30;

/// One message body, or `None` at a clean end of stream.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = String::new();
    let mut first = true;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            if first {
                return Ok(None);
            }
            return Err(ErrorKind::UnexpectedEof.into());
        }
        first = false;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let parsed = value.trim().parse::<usize>().map_err(invalid)?;
            length = Some(parsed);
        }
    }
    let length = length.ok_or_else(|| invalid("missing Content-Length"))?;
    if length > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    frame
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    async fn read_all(mut bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut reader = BufReader::new(&mut bytes);
        let mut frames = Vec::new();
        while let Some(frame) = read_frame(&mut reader).await? {
            frames.push(frame);
        }
        Ok(frames)
    }

    #[tokio::test]
    async fn reads_frames_back_to_back_with_any_header_order() {
        let mut bytes = encode_frame(b"{}");
        bytes.extend_from_slice(b"Content-Type: x\r\ncontent-length: 3\r\n\r\nabc");
        assert_eq!(read_all(&bytes).await.unwrap(), [&b"{}"[..], &b"abc"[..]]);
    }

    #[tokio::test]
    async fn length_counts_bytes_not_characters() {
        let bytes = encode_frame("ж😀".as_bytes());
        assert_eq!(read_all(&bytes).await.unwrap()[0], "ж😀".as_bytes());
    }

    #[tokio::test]
    async fn broken_headers_are_errors() {
        assert!(read_all(b"X: 1\r\n\r\n").await.is_err());
        assert!(read_all(b"Content-Length: x\r\n\r\n").await.is_err());
        assert!(read_all(b"Content-Length: 5\r\n\r\nab").await.is_err());
        assert!(read_all(b"Content-Length: 2").await.is_err());
    }
}
