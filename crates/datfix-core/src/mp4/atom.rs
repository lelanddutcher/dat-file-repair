//! An in-memory tree of MP4 boxes ("atoms").
//!
//! The reference clip's `moov` box is parsed into this tree. The writer then clones the tree,
//! replaces the few boxes that describe sample positions and durations, and serializes it.
//! Every box this crate does not understand stays a [`Body::Leaf`] and is written back byte
//! for byte.

use crate::fourcc::FourCC;
use crate::read_at::ReadAt;

use super::bytes::PutBe;

/// Box types whose payload is itself a list of boxes. Everything else is kept as raw bytes.
/// `udta` and `uuid` are deliberately *not* listed: they are copied verbatim.
const CONTAINERS: [&[u8; 4]; 7] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf",
];

/// One box.
#[derive(Clone, Debug, PartialEq)]
pub struct Atom {
    pub kind: FourCC,
    pub body: Body,
}

/// The payload of a box: raw bytes or child boxes.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Leaf(Vec<u8>),
    Container(Vec<Atom>),
}

impl Atom {
    /// A box holding raw bytes.
    pub fn leaf(kind: &[u8; 4], payload: Vec<u8>) -> Atom {
        Atom {
            kind: FourCC::new(kind),
            body: Body::Leaf(payload),
        }
    }

    /// A box holding other boxes.
    pub fn container(kind: &[u8; 4], children: Vec<Atom>) -> Atom {
        Atom {
            kind: FourCC::new(kind),
            body: Body::Container(children),
        }
    }

    /// Raw payload of a leaf box.
    pub fn payload(&self) -> Option<&[u8]> {
        match &self.body {
            Body::Leaf(p) => Some(p),
            Body::Container(_) => None,
        }
    }

    /// Child boxes of a container (empty for a leaf).
    pub fn children(&self) -> &[Atom] {
        match &self.body {
            Body::Container(c) => c,
            Body::Leaf(_) => &[],
        }
    }

    /// Mutable child list. Panics on a leaf; callers only use it on known containers.
    pub fn children_mut(&mut self) -> &mut Vec<Atom> {
        match &mut self.body {
            Body::Container(c) => c,
            Body::Leaf(_) => panic!("{} is not a container box", self.kind),
        }
    }

    /// First direct child of the given type.
    pub fn child(&self, kind: &[u8; 4]) -> Option<&Atom> {
        self.children().iter().find(|a| a.kind.0 == *kind)
    }

    /// Follows a path of child types, for example `["mdia", "minf", "stbl"]`.
    pub fn find(&self, path: &[&[u8; 4]]) -> Option<&Atom> {
        let mut cur = self;
        for kind in path {
            cur = cur.child(kind)?;
        }
        Some(cur)
    }

    /// Mutable version of [`Atom::find`].
    pub fn find_mut(&mut self, path: &[&[u8; 4]]) -> Option<&mut Atom> {
        let mut cur = self;
        for kind in path {
            let Body::Container(children) = &mut cur.body else {
                return None;
            };
            cur = children.iter_mut().find(|a| a.kind.0 == **kind)?;
        }
        Some(cur)
    }

    /// Payload length in bytes when serialized.
    fn payload_len(&self) -> u64 {
        match &self.body {
            Body::Leaf(p) => p.len() as u64,
            Body::Container(c) => c.iter().map(Atom::encoded_len).sum(),
        }
    }

    /// Total serialized length, header included. Boxes over 4 GiB get a 16-byte header.
    pub fn encoded_len(&self) -> u64 {
        let p = self.payload_len();
        if p + 8 > u32::MAX as u64 {
            p + 16
        } else {
            p + 8
        }
    }

    /// Appends the serialized box to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        let total = self.encoded_len();
        if total > u32::MAX as u64 {
            out.put_u32(1);
            out.extend_from_slice(&self.kind.0);
            out.put_u64(total);
        } else {
            out.put_u32(total as u32);
            out.extend_from_slice(&self.kind.0);
        }
        match &self.body {
            Body::Leaf(p) => out.extend_from_slice(p),
            Body::Container(c) => c.iter().for_each(|a| a.write_to(out)),
        }
    }

    /// Serializes the box into a new vector.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.encoded_len() as usize);
        self.write_to(&mut v);
        v
    }
}

/// Parses a byte slice that holds a sequence of boxes.
pub fn parse_atoms(data: &[u8]) -> Result<Vec<Atom>, String> {
    let mut atoms = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        // Some writers pad the end of a container with fewer than 8 zero bytes.
        if data.len() - pos < 8 {
            if data[pos..].iter().all(|&b| b == 0) {
                break;
            }
            return Err(format!(
                "{} stray bytes after the last box",
                data.len() - pos
            ));
        }
        let size32 = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let kind = FourCC::from_slice(&data[pos + 4..]);
        let (size, header) = match size32 {
            0 => ((data.len() - pos) as u64, 8),
            1 => {
                if data.len() - pos < 16 {
                    return Err(format!("box {kind} has a truncated 64-bit size"));
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&data[pos + 8..pos + 16]);
                (u64::from_be_bytes(b), 16)
            }
            n => (n as u64, 8),
        };
        if size < header as u64 || pos as u64 + size > data.len() as u64 {
            return Err(format!(
                "box {kind} at position {pos} claims {size} bytes but only {} are left",
                data.len() - pos
            ));
        }
        let payload = &data[pos + header..pos + size as usize];
        let body = if CONTAINERS.iter().any(|c| **c == kind.0) {
            Body::Container(parse_atoms(payload).map_err(|e| format!("inside {kind}: {e}"))?)
        } else {
            Body::Leaf(payload.to_vec())
        };
        atoms.push(Atom { kind, body });
        pos += size as usize;
    }
    Ok(atoms)
}

/// Position of a top-level box inside a file. Used to find `ftyp`, `moov` and `mdat` without
/// reading the (possibly huge) media payload.
#[derive(Clone, Debug)]
pub struct TopBox {
    pub kind: FourCC,
    pub offset: u64,
    pub header_len: u64,
    pub size: u64,
}

/// Lists the top-level boxes of a file by hopping from header to header.
///
/// Stops at the end of the file. A last box that claims more bytes than the file has is still
/// listed (a reference copy may have been cut short after its `moov`).
pub fn scan_top_level<R: ReadAt + ?Sized>(src: &R) -> std::io::Result<Vec<TopBox>> {
    let len = src.size();
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= len {
        let mut h = [0u8; 16];
        let avail = (len - pos).min(16) as usize;
        src.read_exact_at(&mut h[..avail], pos)?;
        let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let kind = FourCC::from_slice(&h[4..8]);
        let (size, header_len) = match size32 {
            0 => (len - pos, 8),
            1 if avail == 16 => (u64::from_be_bytes(h[8..16].try_into().unwrap()), 16),
            1 => break,
            n => (n as u64, 8),
        };
        if size < header_len || !kind.is_printable() {
            break;
        }
        out.push(TopBox {
            kind,
            offset: pos,
            header_len,
            size,
        });
        pos = pos.saturating_add(size);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_nested_boxes() {
        let tree = Atom::container(
            b"moov",
            vec![
                Atom::leaf(b"mvhd", vec![1, 2, 3]),
                Atom::container(b"trak", vec![Atom::leaf(b"tkhd", vec![9; 10])]),
                Atom::leaf(b"udta", vec![0, 0, 0, 8, b'f', b'r', b'e', b'e']),
            ],
        );
        let bytes = tree.to_bytes();
        assert_eq!(bytes.len() as u64, tree.encoded_len());
        let parsed = parse_atoms(&bytes).unwrap();
        assert_eq!(parsed, vec![tree.clone()]);
        assert_eq!(
            parsed[0]
                .find(&[b"trak", b"tkhd"])
                .unwrap()
                .payload()
                .unwrap(),
            &[9; 10]
        );
    }

    #[test]
    fn rejects_box_that_overruns_parent() {
        let mut bytes = Atom::leaf(b"abcd", vec![0; 4]).to_bytes();
        bytes[3] = 100;
        assert!(parse_atoms(&bytes).is_err());
    }

    #[test]
    fn scans_top_level_with_large_size() {
        let mut file = Atom::leaf(b"ftyp", vec![0; 4]).to_bytes();
        // 64-bit mdat header that claims more than the buffer holds.
        file.extend_from_slice(&[0, 0, 0, 1]);
        file.extend_from_slice(b"mdat");
        file.extend_from_slice(&(1u64 << 33).to_be_bytes());
        let boxes = scan_top_level(&file).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].kind, FourCC::new(b"mdat"));
        assert_eq!(boxes[1].header_len, 16);
        assert_eq!(boxes[1].size, 1 << 33);
    }
}
