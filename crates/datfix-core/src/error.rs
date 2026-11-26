//! Error type shared by the whole crate.
//!
//! Every message is written for the person using the tool, not for a developer: it says what
//! went wrong and, where possible, what to try next.

use std::path::PathBuf;

/// Result alias used across the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can stop an inspection or a repair.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The operating system refused a read or write.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// The `.DAT` file does not contain a usable Canon recovery index.
    #[error("{0}")]
    NotADat(String),

    /// The reference clip is not a readable MP4.
    #[error("the reference clip {path} cannot be used: {reason}")]
    BadReference { path: PathBuf, reason: String },

    /// The reference clip is a valid MP4 but was recorded with different settings.
    #[error(
        "the reference clip {path} does not match this .DAT:\n{}\nPick a healthy clip recorded on the same camera with the same resolution, frame rate, codec and audio settings.",
        .reasons.iter().map(|r| format!("  - {r}")).collect::<Vec<_>>().join("\n")
    )]
    Incompatible { path: PathBuf, reasons: Vec<String> },

    /// No file in a reference folder matched.
    #[error(
        "no compatible reference clip found in {}.\n{}",
        .dir.display(),
        .tried.iter().map(|(p, r)| format!("  - {}: {r}", p.display())).collect::<Vec<_>>().join("\n")
    )]
    NoReference {
        dir: PathBuf,
        tried: Vec<(PathBuf, String)>,
    },

    /// The `.DAT` is empty, or holds only zero bytes.
    #[error(
        "{} holds no recording data ({0}). The data isn't in this file: the camera's file \
         table points at nothing. Recover the memory card with PhotoRec (free, \
         https://www.cgsecurity.org) first, without writing anything to the card, then run \
         datfix on the .DAT or .MP4 pieces it finds.",
        .path.display()
    )]
    EmptyDat { path: PathBuf, detail: String },

    /// The file is not a Canon camera recording.
    #[error("this isn't a Canon camera file: {0}")]
    NotCanon(String),

    /// A Canon recording that this version cannot repair.
    #[error("{0}")]
    Unsupported(String),

    /// No built-in profile fits and no reference was given.
    #[error(
        "no built-in profile fits this .DAT ({summary}).\n{}\nPass --reference with a healthy MP4 \
         recorded on the same camera with the same resolution, frame rate and codec.",
        .tried.iter().map(|(p, r)| format!("  - {p}: {r}")).collect::<Vec<_>>().join("\n")
    )]
    NoProfile {
        summary: String,
        tried: Vec<(String, String)>,
    },

    /// Several built-in profiles fit and they would decode differently.
    #[error(
        "several built-in profiles fit this .DAT ({summary}) and the frames alone cannot tell \
         them apart:\n{}\nPass --reference with a healthy clip from the same camera and mode.",
        .candidates.iter().map(|c| format!("  - {c}")).collect::<Vec<_>>().join("\n")
    )]
    AmbiguousProfile {
        summary: String,
        candidates: Vec<String>,
    },

    /// The output already exists and the caller did not allow overwriting it.
    #[error("{0} already exists. Choose another output name, or pass --force to replace it.")]
    OutputExists(PathBuf),

    /// The output path is the input path.
    #[error("the output path is the same file as the input. The input is never modified.")]
    OutputIsInput,

    /// The caller asked to stop.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// Wraps an I/O error with a short description of what was being done.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}
