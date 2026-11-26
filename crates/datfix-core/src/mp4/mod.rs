//! MP4 (ISO base media file format) reading and writing.
//!
//! An MP4 file is a list of *boxes*. Each box starts with a 32-bit big-endian size and a
//! four-character type. Some boxes (`moov`, `trak`, ...) contain other boxes; the rest hold
//! data. Only the boxes needed for a repair are interpreted; everything else is copied as raw
//! bytes.

pub mod atom;
pub mod bytes;
pub mod reference;
pub mod tables;
pub mod writer;
