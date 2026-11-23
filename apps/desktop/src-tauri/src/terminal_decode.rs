//! Incremental UTF-8 decoder for PTY output. Read boundaries can split Unicode
//! code points; decoding each chunk with from_utf8_lossy corrupts output.

#[derive(Default)]
pub(crate) struct TerminalUtf8Decoder {
    pending: Vec<u8>,
}

impl TerminalUtf8Decoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        let mut offset = 0;
        while offset < self.pending.len() {
            match std::str::from_utf8(&self.pending[offset..]) {
                Ok(valid) => {
                    output.push_str(valid);
                    offset = self.pending.len();
                }
                Err(error) => {
                    let valid_end = offset + error.valid_up_to();
                    let prefix = std::str::from_utf8(&self.pending[offset..valid_end])
                        .expect("valid_up_to is a valid UTF-8 prefix");
                    output.push_str(prefix);
                    match error.error_len() {
                        Some(bad_len) => {
                            output.push('\u{fffd}');
                            offset = valid_end + bad_len;
                        }
                        None => {
                            offset = valid_end;
                            break;
                        }
                    }
                }
            }
        }
        if offset > 0 {
            self.pending.drain(..offset);
        }
        output
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        self.pending.clear();
        "\u{fffd}".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalUtf8Decoder;

    #[test]
    fn preserves_multibyte_text_across_read_boundaries() {
        let mut decoder = TerminalUtf8Decoder::default();
        let bytes = "Shell λ 🚀".as_bytes();
        let mut decoded = String::new();
        for byte in bytes {
            decoded.push_str(&decoder.push(&[*byte]));
        }
        decoded.push_str(&decoder.finish());
        assert_eq!(decoded, "Shell λ 🚀");
    }

    #[test]
    fn replaces_malformed_bytes_without_dropping_following_text() {
        let mut decoder = TerminalUtf8Decoder::default();
        assert_eq!(decoder.push(b"ok\xffdone"), "ok\u{fffd}done");
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn replaces_an_incomplete_final_codepoint_only_at_eof() {
        let mut decoder = TerminalUtf8Decoder::default();
        assert_eq!(decoder.push(&[0xf0, 0x9f]), "");
        assert_eq!(decoder.finish(), "\u{fffd}");
    }

    #[test]
    fn never_keeps_complete_valid_bytes_buffered() {
        let mut decoder = TerminalUtf8Decoder::default();
        for _ in 0..50 {
            assert_eq!(decoder.push(b"abc"), "abc");
            assert!(decoder.pending.is_empty());
        }
    }
}
