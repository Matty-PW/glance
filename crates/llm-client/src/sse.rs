//! Minimal server-sent-events framing: bytes in, `data:` payloads out.

/// A single line longer than this means the peer is not speaking SSE.
const MAX_LINE: usize = 1 << 20;

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LineTooLong;

impl SseParser {
    /// Feed bytes as they arrive. Returns the payload of every complete
    /// `data:` line seen so far; partial lines are kept for the next call.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, LineTooLong> {
        self.buf.extend_from_slice(bytes);
        let mut payloads = Vec::new();
        let mut start = 0;
        while let Some(offset) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let line = &self.buf[start..start + offset];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(data) = line.strip_prefix(b"data:") {
                let text = String::from_utf8_lossy(data);
                payloads.push(text.trim_start().to_owned());
            }
            start += offset + 1;
        }
        self.buf.drain(..start);
        if self.buf.len() > MAX_LINE {
            return Err(LineTooLong);
        }
        Ok(payloads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_and_ignores_other_lines() {
        let mut p = SseParser::default();
        let out = p
            .push(b": comment\nevent: x\ndata: {\"a\":1}\n\ndata:{\"b\":2}\r\n\r\ndata: [DONE]\n\n")
            .unwrap();
        assert_eq!(out, ["{\"a\":1}", "{\"b\":2}", "[DONE]"]);
    }

    #[test]
    fn reassembles_payloads_split_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"da").unwrap().is_empty());
        assert!(p.push(b"ta: {\"text\":\"hel").unwrap().is_empty());
        assert_eq!(p.push(b"lo\"}\n").unwrap(), ["{\"text\":\"hello\"}"]);
        // A multi-byte character split across chunks survives.
        let bytes = "data: \"é\"\n".as_bytes();
        assert!(p.push(&bytes[..8]).unwrap().is_empty());
        assert_eq!(p.push(&bytes[8..]).unwrap(), ["\"é\""]);
    }

    #[test]
    fn rejects_an_endless_line() {
        let mut p = SseParser::default();
        assert_eq!(p.push(&vec![b'x'; MAX_LINE + 1]), Err(LineTooLong));
    }
}
