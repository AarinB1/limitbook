//! Transport framing for ITCH 5.0 capture streams.
//!
//! Nasdaq's downloadable TotalView-ITCH sample files use the BinaryFILE
//! convention: each message payload is preceded by a 2-byte big-endian
//! length. This module only splits a byte slice into message payloads; the
//! payload layouts themselves are defined by `spec/itch50_spec.txt` and are
//! decoded by the (later-phase) parser.
//!
//! Every ITCH 5.0 payload starts with the uniform header — Message Type
//! (offset 0, 1 byte), Stock Locate (offset 1, 2 bytes), Tracking Number
//! (offset 3, 2 bytes), Timestamp (offset 5, 6 bytes) — so any valid payload
//! is at least [`MIN_MESSAGE_LEN`] bytes.

/// Length of the uniform header shared by all ITCH 5.0 messages
/// (type + stock locate + tracking number + timestamp).
pub const MIN_MESSAGE_LEN: usize = 11;

/// A framing error, carrying the byte offset where it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The buffer ended inside a length prefix or message body.
    Truncated { offset: usize },
    /// A length prefix declared a payload shorter than the uniform header.
    TooShort { offset: usize, len: usize },
}

/// Iterator over length-prefixed message payloads in a byte slice.
///
/// Yields one `&[u8]` payload (without the length prefix) per message. On a
/// framing error it yields `Err` once and then terminates, since message
/// boundaries cannot be recovered past a bad frame.
#[derive(Debug, Clone)]
pub struct Messages<'a> {
    buf: &'a [u8],
    offset: usize,
    failed: bool,
}

impl<'a> Messages<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Messages {
            buf,
            offset: 0,
            failed: false,
        }
    }

    /// Byte offset of the next unread length prefix.
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl<'a> Iterator for Messages<'a> {
    type Item = Result<&'a [u8], FrameError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.offset == self.buf.len() {
            return None;
        }
        let start = self.offset;
        let Some(prefix) = self.buf.get(start..start + 2) else {
            self.failed = true;
            return Some(Err(FrameError::Truncated { offset: start }));
        };
        let len = u16::from_be_bytes([prefix[0], prefix[1]]) as usize;
        if len < MIN_MESSAGE_LEN {
            self.failed = true;
            return Some(Err(FrameError::TooShort { offset: start, len }));
        }
        let Some(payload) = self.buf.get(start + 2..start + 2 + len) else {
            self.failed = true;
            return Some(Err(FrameError::Truncated { offset: start }));
        };
        self.offset = start + 2 + len;
        Some(Ok(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 11-byte payload with the given type byte, length-prefixed.
    fn msg(ty: u8) -> [u8; 13] {
        let mut m = [0u8; 13];
        m[1] = 11;
        m[2] = ty;
        m
    }

    #[test]
    fn empty_slice_yields_nothing() {
        assert_eq!(Messages::new(&[]).count(), 0);
    }

    #[test]
    fn splits_consecutive_messages() {
        let mut buf = [0u8; 26];
        buf[..13].copy_from_slice(&msg(b'S'));
        buf[13..].copy_from_slice(&msg(b'R'));
        let mut it = Messages::new(&buf);
        assert_eq!(it.next().unwrap().unwrap()[0], b'S');
        assert_eq!(it.next().unwrap().unwrap()[0], b'R');
        assert!(it.next().is_none());
        assert_eq!(it.offset(), 26);
    }

    #[test]
    fn truncated_length_prefix() {
        let mut it = Messages::new(&[0x00]);
        assert_eq!(it.next(), Some(Err(FrameError::Truncated { offset: 0 })));
        assert!(it.next().is_none(), "iterator fuses after an error");
    }

    #[test]
    fn truncated_body() {
        let buf = &msg(b'S')[..7];
        let mut it = Messages::new(buf);
        assert_eq!(it.next(), Some(Err(FrameError::Truncated { offset: 0 })));
        assert!(it.next().is_none());
    }

    #[test]
    fn error_offset_points_at_bad_frame() {
        let mut buf = [0u8; 15];
        buf[..13].copy_from_slice(&msg(b'S'));
        // Second frame: length prefix present but body missing entirely.
        buf[13] = 0;
        buf[14] = 11;
        let mut it = Messages::new(&buf);
        assert!(it.next().unwrap().is_ok());
        assert_eq!(it.next(), Some(Err(FrameError::Truncated { offset: 13 })));
    }

    #[test]
    fn rejects_length_below_uniform_header() {
        let mut it = Messages::new(&[0x00, 0x01, b'S']);
        assert_eq!(
            it.next(),
            Some(Err(FrameError::TooShort { offset: 0, len: 1 }))
        );
        assert!(it.next().is_none());
    }
}
