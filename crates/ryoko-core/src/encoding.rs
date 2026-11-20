//! Small, dependency-free encodings shared by attestations and vision payloads.

/// Standard base64 with padding, as required by DSSE and by image payloads.
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map(|b| u32::from(*b)).unwrap_or(0);
        let b2 = chunk.get(2).map(|b| u32::from(*b)).unwrap_or(0);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes canonical base64. Non-canonical padding bits, wrong lengths and any
/// foreign character are rejected, so two different strings cannot decode to the
/// same bytes.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    // Bitwise so the check does not depend on `is_multiple_of` (Rust 1.87)
    // while the workspace still supports 1.85.
    if text.len() & 3 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes = text.as_bytes();
    let chunks = bytes.len() / 4;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let pad = chunk.iter().filter(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && index != chunks - 1) {
            return None;
        }
        let mut triple = 0u32;
        for (offset, byte) in chunk.iter().enumerate() {
            let digit = if *byte == b'=' { 0 } else { value(*byte)? };
            triple |= digit << (18 - 6 * offset);
        }
        if (pad == 2 && triple & 0x0000_ffff != 0) || (pad == 1 && triple & 0x0000_00ff != 0) {
            return None;
        }
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_length_and_rejects_garbage() {
        for length in 0..40usize {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 37 % 251) as u8).collect();
            let encoded = base64_encode(&bytes);
            assert_eq!(base64_decode(&encoded).unwrap(), bytes);
        }
        assert_eq!(base64_decode("QQ==").unwrap(), b"A".to_vec());
        assert!(base64_decode("not base64!!").is_none());
        assert!(
            base64_decode("QQ=").is_none(),
            "length must be a multiple of 4"
        );
        assert!(base64_decode("QR==").is_none(), "non-zero padding bits");
    }
}
