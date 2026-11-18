//! The part of RFC 6455 the IDE protocol needs: one upgrade handshake, text frames and
//! the control frames of a quiet local connection. No extensions and no compression are
//! negotiated, so both sides may send plain frames only.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use base64::Engine;
use sha1_smol::Sha1;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_HEADERS: usize = 16 * 1024;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

pub(crate) struct Frame {
    pub(crate) fin: bool,
    pub(crate) opcode: Opcode,
    pub(crate) payload: Vec<u8>,
}

fn accept_key(key: &str) -> String {
    let mut digest = Sha1::new();
    digest.update(key.as_bytes());
    digest.update(GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest.digest().bytes())
}

/// The value of a header, by its lower-case name.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, value)| value.as_str())
}

fn read_until_headers(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while raw.len() < MAX_HEADERS {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the request ended before its headers",
            ));
        }
        raw.push(byte[0]);
        if raw.ends_with(b"\r\n\r\n") {
            return Ok(raw);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the request headers are too long",
    ))
}

/// Answers the upgrade. `Ok(false)` means the request was refused for a reason the
/// client can read; the caller only closes the socket.
pub(crate) fn handshake(stream: &mut TcpStream, token: &str) -> io::Result<bool> {
    let raw = read_until_headers(stream)?;
    let text = String::from_utf8_lossy(&raw);
    let mut lines = text.lines();
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let upgrade = header(&headers, "upgrade").unwrap_or_default();
    let connection = header(&headers, "connection").unwrap_or_default();
    let protocols = header(&headers, "sec-websocket-protocol").unwrap_or_default();
    let key = header(&headers, "sec-websocket-key");
    let version = header(&headers, "sec-websocket-version").unwrap_or_default();
    let given = header(&headers, "x-claude-code-ide-authorization");

    let upgrades = upgrade.eq_ignore_ascii_case("websocket") && connection.contains("Upgrade");
    let speaks_mcp = protocols.split(',').any(|name| name.trim() == "mcp");
    let allowed = given.is_some_and(|given| given == token);
    if method != "GET" || !upgrades || !speaks_mcp || version != "13" || !allowed {
        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")?;
        return Ok(false);
    }
    let Some(key) = key else {
        stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")?;
        return Ok(false);
    };
    let answer = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: mcp\r\n\r\n",
        accept_key(key)
    );
    stream.write_all(answer.as_bytes())?;
    stream.flush()?;
    Ok(true)
}

fn opcode_of(byte: u8) -> io::Result<Opcode> {
    Ok(match byte {
        0x0 => Opcode::Continuation,
        0x1 => Opcode::Text,
        0x2 => Opcode::Binary,
        0x8 => Opcode::Close,
        0x9 => Opcode::Ping,
        0xa => Opcode::Pong,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an unknown opcode",
            ));
        }
    })
}

/// One frame off the wire. `Ok(None)` is the end of the stream.
pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 2];
    match reader.read_exact(&mut head) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let fin = head[0] & 0x80 != 0;
    let opcode = opcode_of(head[0] & 0x0f)?;
    let masked = head[1] & 0x80 != 0;
    if !masked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a client frame must be masked",
        ));
    }
    let length = head[1] & 0x7f;
    let length = match length {
        126 => {
            let mut raw = [0u8; 2];
            reader.read_exact(&mut raw)?;
            u16::from_be_bytes(raw) as usize
        }
        127 => {
            let mut raw = [0u8; 8];
            reader.read_exact(&mut raw)?;
            let value = u64::from_be_bytes(raw);
            usize::try_from(value)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a frame is too long"))?
        }
        other => other as usize,
    };
    if length > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a frame is too long",
        ));
    }
    let mut mask = [0u8; 4];
    reader.read_exact(&mut mask)?;
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Ok(Some(Frame {
        fin,
        opcode,
        payload,
    }))
}

fn send(writer: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        length if length < 126 => frame.push(length as u8),
        length if length <= u16::MAX as usize => {
            frame.push(126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            frame.push(127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()
}

pub(crate) fn send_text(writer: &mut impl Write, text: &str) -> io::Result<()> {
    send(writer, 0x1, text.as_bytes())
}

pub(crate) fn send_pong(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    send(writer, 0xa, payload)
}

pub(crate) fn send_close(writer: &mut impl Write) -> io::Result<()> {
    send(writer, 0x8, &1000u16.to_be_bytes())
}

/// A masked frame as a client sends it, for the tests.
#[cfg(test)]
pub(crate) fn client_frame(text: &str, mask: [u8; 4]) -> Vec<u8> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81);
    match payload.len() {
        length if length < 126 => frame.push(0x80 | length as u8),
        length if length <= u16::MAX as usize => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    for (index, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[index % 4]);
    }
    frame
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn the_accept_value_is_the_one_of_the_rfc() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";

        assert_eq!(accept_key(key), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn a_masked_text_frame_comes_back_whole() {
        let mut wire = Cursor::new(client_frame("{\"id\":1}", [1, 2, 3, 4]));

        let frame = read_frame(&mut wire).unwrap().unwrap();

        assert_eq!(frame.opcode, Opcode::Text);
        assert!(frame.fin);
        assert_eq!(frame.payload, b"{\"id\":1}");
    }

    #[test]
    fn a_long_frame_takes_the_extended_length() {
        let text = "x".repeat(400);
        let mut wire = Cursor::new(client_frame(&text, [9, 8, 7, 6]));

        let frame = read_frame(&mut wire).unwrap().unwrap();

        assert_eq!(frame.payload, text.as_bytes());
    }

    #[test]
    fn an_unmasked_client_frame_is_refused() {
        let mut wire = Cursor::new(vec![0x81, 0x02, b'h', b'i']);

        assert!(read_frame(&mut wire).is_err());
    }

    #[test]
    fn our_text_frame_is_readable_by_the_client_rules() {
        let mut wire = Cursor::new(Vec::new());
        send_text(&mut wire, "hello").unwrap();
        let bytes = wire.into_inner();

        assert_eq!(bytes[0], 0x81);
        assert_eq!(bytes[1], 5, "the server never masks");
        assert_eq!(&bytes[2..], b"hello");
    }
}
