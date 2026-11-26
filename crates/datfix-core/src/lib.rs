//! `datfix-core` rebuilds a playable MP4 from an interrupted Canon `.DAT` recording.
//!
//! # Background
//!
//! A Canon camera streams media into a temporary `.DAT` file while it records. It writes the
//! MP4 index (`moov`) only when recording stops. If the battery dies or the card errors, the
//! index is never written and players cannot open the file.
//!
//! Canon cameras also write a small recovery index into the media stream itself: every group
//! of samples starts with a `repi` header that lists the sample type and the exact size of each
//! sample. This crate walks those headers ([`repi`]), takes the codec settings from a healthy
//! clip recorded in the same mode ([`mp4::reference`]), and writes a new MP4 whose `mdat`
//! payload is the `.DAT` bytes copied unchanged ([`repair`]).
//!
//! # Entry points
//!
//! - [`inspect::inspect`] summarizes a `.DAT` without a reference.
//! - [`repair::repair`] writes a repaired MP4. It reports progress through a callback so a GUI
//!   can show a progress bar.
//! - [`repair::choose_reference`] picks a compatible reference from a folder.
//!
//! The input file is only ever opened for reading.

pub mod codec;
pub mod error;
pub mod fourcc;
pub mod hybrid;
pub mod inspect;
pub mod layout;
pub mod mapping;
pub mod mp4;
pub mod profile;
pub mod read_at;
pub mod repair;
pub mod repi;
pub mod select;
pub mod timeline;
pub mod timing;

pub use error::{Error, Result};
pub use fourcc::FourCC;
