use std::io::{self, BufRead};

/// Hard cap on one language-server frame; prevents unbounded allocation from a
/// misbehaving child process.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub fn frame(message: &str) -> Vec<u8> {
    let mut bytes = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
    bytes.extend_from_slice(message.as_bytes());
    bytes
}

/// Reads one `Content-Length` framed message. `Ok(None)` means the pipe closed
/// cleanly between frames.
pub fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            let parsed = value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?;
            length = Some(parsed);
        }
    }
    let length = length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "language-server frame exceeds the size limit",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_and_reads_messages_round_trip() {
        let mut pipe = Cursor::new(frame("{\"jsonrpc\":\"2.0\",\"id\":1}"));
        let body = read_frame(&mut pipe).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"jsonrpc\":\"2.0\",\"id\":1}"
        );
        assert!(read_frame(&mut pipe).unwrap().is_none());
    }

    #[test]
    fn accepts_lowercase_headers_and_crlf_frames() {
        let mut pipe = Cursor::new(b"content-length: 2\r\n\r\nok".to_vec());
        assert_eq!(read_frame(&mut pipe).unwrap().unwrap(), b"ok");
    }

    #[test]
    fn rejects_missing_or_oversized_lengths() {
        let mut missing = Cursor::new(b"X-Other: 1\r\n\r\n".to_vec());
        assert!(read_frame(&mut missing).is_err());

        let oversized = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
        let mut pipe = Cursor::new(oversized.into_bytes());
        assert!(read_frame(&mut pipe).is_err());
    }
}
