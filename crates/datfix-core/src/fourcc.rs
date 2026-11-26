//! Four-character codes such as `moov`, `hvc1` or `lpcm`.

use std::fmt;

/// A four-byte tag used by MP4 box types, codec names and `repi` sample types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCC(pub [u8; 4]);

impl FourCC {
    /// Builds a code from a byte-string literal, for example `FourCC::new(b"moov")`.
    pub const fn new(bytes: &[u8; 4]) -> Self {
        FourCC(*bytes)
    }

    /// Builds a code from the first four bytes of a slice. Panics if the slice is shorter.
    pub fn from_slice(bytes: &[u8]) -> Self {
        FourCC([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// True when every byte is printable ASCII (space included).
    pub fn is_printable(&self) -> bool {
        self.0.iter().all(|b| (0x20..=0x7e).contains(b))
    }
}

impl fmt::Display for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in &self.0 {
            if (0x20..=0x7e).contains(&b) {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{self}'")
    }
}

impl serde::Serialize for FourCC {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_escapes_non_printable_bytes() {
        assert_eq!(FourCC::new(b"aac ").to_string(), "aac ");
        assert_eq!(FourCC([b'a', 0, b'b', 0xff]).to_string(), "a\\x00b\\xff");
        assert!(!FourCC([b'a', 0, b'b', 0xff]).is_printable());
    }
}
