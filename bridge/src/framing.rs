//! LSP Content-Length framing — encode frames, and incrementally split a byte
//! stream into complete frames.

/// Encodes one JSON body as a `Content-Length` framed LSP message.
pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Accumulates bytes and yields complete `Content-Length` frames.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Returns the next complete frame body, if one is fully buffered.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let header_end = find_subslice(&self.buf, b"\r\n\r\n")?;
        let header = std::str::from_utf8(&self.buf[..header_end]).ok()?;
        let len = content_length(header)?;
        let body_start = header_end + 4;
        if self.buf.len() < body_start + len {
            return None;
        }
        let body = self.buf[body_start..body_start + len].to_vec();
        self.buf.drain(..body_start + len);
        Some(body)
    }
}

/// Finds `needle` inside `haystack` (naive byte scan — headers are tiny).
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extracts `Content-Length` from an LSP/HTTP header block.
pub fn content_length(header: &str) -> Option<usize> {
    for line in header.split("\r\n") {
        let trimmed = line.trim();
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            return value.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let frame = encode_frame(body);
        let expected_header = format!("Content-Length: {}\r\n\r\n", body.len());
        assert!(frame.starts_with(expected_header.as_bytes()));
        let mut reader = FrameReader::new();
        // Feed it byte-by-byte to exercise incremental parsing.
        for b in frame {
            reader.push(&[b]);
            if let Some(out) = reader.next_frame() {
                assert_eq!(out, body);
                return;
            }
        }
        panic!("frame never completed");
    }

    #[test]
    fn multiple_frames_in_one_chunk() {
        let mut reader = FrameReader::new();
        let f1 = encode_frame(br#"{"a":1}"#);
        let f2 = encode_frame(br#"{"b":2}"#);
        let mut all = f1.clone();
        all.extend_from_slice(&f2);
        reader.push(&all);
        assert_eq!(reader.next_frame().unwrap(), br#"{"a":1}"#);
        assert_eq!(reader.next_frame().unwrap(), br#"{"b":2}"#);
        assert!(reader.next_frame().is_none());
    }
}
