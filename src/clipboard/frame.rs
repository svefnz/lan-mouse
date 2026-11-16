//! Wire framing for clipboard payloads.
//!
//! Clipboard content does not fit the fixed 13-byte peer protocol, so it
//! travels on its own TLS-over-TCP channel as length-prefixed frames:
//!
//! ```text
//! u32 frame_len (big endian)
//!   u64 origin_seq
//!   u16 n_items
//!   n_items * { u16 name_len, name, u32 data_len, data }
//! ```
//!
//! The encoding is hand-rolled rather than going through serde: a
//! snapshot can hold tens of megabytes and must not be copied again just
//! to be framed.

use input_clipboard::Representation;

/// Largest value a frame length field can express.
pub const MAX_FRAME_LEN: usize = u32::MAX as usize;

#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    /// Sender's local clipboard change counter, used by the receiver to
    /// recognize the echo of a write it just applied.
    pub origin_seq: u64,
    pub items: Vec<Representation>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    /// the buffer does not hold a complete frame yet
    #[error("incomplete frame")]
    Incomplete,
    /// the declared length exceeds the configured limit
    #[error("frame of {0} bytes exceeds the limit of {1}")]
    TooLarge(usize, usize),
    /// the payload is structurally invalid
    #[error("malformed frame: {0}")]
    Malformed(&'static str),
}

/// Encode a frame including its length prefix.
///
/// Items whose name cannot be expressed in the length field are dropped;
/// a clipboard type name is never anywhere near that long.
pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + 2 + frame.items.len() * 8);
    payload.extend_from_slice(&frame.origin_seq.to_be_bytes());

    let items: Vec<&Representation> = frame
        .items
        .iter()
        .filter(|i| u16::try_from(i.name.len()).is_ok())
        .collect();
    payload.extend_from_slice(&(items.len() as u16).to_be_bytes());

    for item in items {
        payload.extend_from_slice(&(item.name.len() as u16).to_be_bytes());
        payload.extend_from_slice(item.name.as_bytes());
        payload.extend_from_slice(&(item.bytes.len() as u32).to_be_bytes());
        payload.extend_from_slice(&item.bytes);
    }

    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Read a length prefix without consuming the body.
///
/// Returns the total frame size (prefix included) once the prefix is
/// present. `max_len` is checked before anything is allocated.
pub fn peek_len(buf: &[u8], max_len: usize) -> Result<usize, DecodeError> {
    if buf.len() < 4 {
        return Err(DecodeError::Incomplete);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > max_len {
        return Err(DecodeError::TooLarge(len, max_len));
    }
    Ok(4 + len)
}

/// Decode one frame, returning it and the number of bytes consumed.
///
/// A frame that is only partially present yields
/// [`DecodeError::Incomplete`], never a partial item list.
pub fn decode(buf: &[u8], max_len: usize) -> Result<(Frame, usize), DecodeError> {
    let total = peek_len(buf, max_len)?;
    if buf.len() < total {
        return Err(DecodeError::Incomplete);
    }
    Ok((decode_payload(&buf[4..total])?, total))
}

/// Decode a frame body, without its length prefix.
pub fn decode_payload(payload: &[u8]) -> Result<Frame, DecodeError> {
    let mut cursor = Cursor {
        buf: payload,
        pos: 0,
    };
    let origin_seq = cursor.u64()?;
    let n_items = cursor.u16()? as usize;

    let mut items = Vec::with_capacity(n_items.min(64));
    for _ in 0..n_items {
        let name_len = cursor.u16()? as usize;
        let name = cursor.bytes(name_len)?;
        let name = std::str::from_utf8(name)
            .map_err(|_| DecodeError::Malformed("representation name is not utf-8"))?
            .to_owned();
        let data_len = cursor.u32()? as usize;
        let bytes = cursor.bytes(data_len)?.to_vec();
        items.push(Representation::new(name, bytes));
    }

    if cursor.pos != cursor.buf.len() {
        return Err(DecodeError::Malformed("trailing bytes after the last item"));
    }

    Ok(Frame { origin_seq, items })
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(DecodeError::Malformed("length overflow"))?;
        if end > self.buf.len() {
            return Err(DecodeError::Malformed("item length exceeds frame"));
        }
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.bytes(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(name: &str, bytes: &[u8]) -> Representation {
        Representation::new(name, bytes.to_vec())
    }

    const LIMIT: usize = 1 << 20;

    #[test]
    fn round_trips_no_items() {
        let frame = Frame {
            origin_seq: 0,
            items: vec![],
        };
        let buf = encode(&frame);
        let (decoded, consumed) = decode(&buf, LIMIT).expect("decodes");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn round_trips_one_item() {
        let frame = Frame {
            origin_seq: 42,
            items: vec![rep("text/plain", b"hello")],
        };
        let buf = encode(&frame);
        let (decoded, _) = decode(&buf, LIMIT).expect("decodes");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn round_trips_mixed_name_and_data_lengths() {
        let frame = Frame {
            origin_seq: u64::MAX,
            items: vec![
                rep("a", b""),
                rep("text/html", &[0u8; 300]),
                rep("com.example.very.long.private.type.name", b"\xff\x00\xfe"),
            ],
        };
        let buf = encode(&frame);
        let (decoded, consumed) = decode(&buf, LIMIT).expect("decodes");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn oversized_declared_length_is_rejected_before_allocating() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        // no body at all - the length alone must be enough to reject it
        assert_eq!(
            decode(&buf, 1024),
            Err(DecodeError::TooLarge(u32::MAX as usize, 1024))
        );
        assert_eq!(
            peek_len(&buf, 1024),
            Err(DecodeError::TooLarge(u32::MAX as usize, 1024))
        );
    }

    #[test]
    fn truncated_prefix_is_incomplete() {
        assert_eq!(decode(&[0, 0], LIMIT), Err(DecodeError::Incomplete));
        assert_eq!(peek_len(&[], LIMIT), Err(DecodeError::Incomplete));
    }

    #[test]
    fn truncated_body_is_incomplete_and_yields_no_items() {
        let frame = Frame {
            origin_seq: 7,
            items: vec![rep("text/plain", b"hello world")],
        };
        let buf = encode(&frame);
        for cut in 4..buf.len() {
            assert_eq!(
                decode(&buf[..cut], LIMIT),
                Err(DecodeError::Incomplete),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn a_short_item_body_is_malformed_not_truncated() {
        // frame declares a 100 byte item but only carries 3
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u64.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&3u16.to_be_bytes());
        payload.extend_from_slice(b"abc");
        payload.extend_from_slice(&100u32.to_be_bytes());
        payload.extend_from_slice(b"xyz");

        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(&payload);

        assert_eq!(
            decode(&buf, LIMIT),
            Err(DecodeError::Malformed("item length exceeds frame"))
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let frame = Frame {
            origin_seq: 1,
            items: vec![rep("text/plain", b"hi")],
        };
        let mut buf = encode(&frame);
        let body_len = buf.len() as u32 - 4;
        buf.extend_from_slice(&[0]);
        buf[..4].copy_from_slice(&(body_len + 1).to_be_bytes());
        assert_eq!(
            decode(&buf, LIMIT),
            Err(DecodeError::Malformed("trailing bytes after the last item"))
        );
    }

    #[test]
    fn a_second_frame_is_left_in_the_buffer() {
        let a = Frame {
            origin_seq: 1,
            items: vec![rep("text/plain", b"a")],
        };
        let b = Frame {
            origin_seq: 2,
            items: vec![rep("text/plain", b"b")],
        };
        let mut buf = encode(&a);
        let first_len = buf.len();
        buf.extend_from_slice(&encode(&b));

        let (decoded_a, consumed) = decode(&buf, LIMIT).expect("first");
        assert_eq!(decoded_a, a);
        assert_eq!(consumed, first_len);
        let (decoded_b, _) = decode(&buf[consumed..], LIMIT).expect("second");
        assert_eq!(decoded_b, b);
    }
}
