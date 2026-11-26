//! Error type shared by the whole crate.
//!
//! Every error has two texts:
//!
//! - `Display` (`to_string()`) is the **technical detail**: exact paths, byte offsets, codec
//!   names. It goes into logs, bug reports and behind a "Details" button.
//! - [`Error::user_message`] is the **plain-words message**: a title, what happened and what to
//!   do next, with no format jargon. The CLI and the desktop app both show this one first.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Where people report cameras that do not work yet.
pub const ISSUES_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues");

/// Why a candidate reference (a clip or a built-in profile) does not fit a `.DAT`.
///
/// `user` is one plain sentence for the screen. `detail` is the technical reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub user: String,
    pub detail: String,
}

impl Reason {
    pub fn new(user: impl Into<String>, detail: impl Into<String>) -> Self {
        Reason {
            user: user.into(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// An error in plain words, ready to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UserMessage {
    /// Stable code, for example `empty_dat` or `no_profile`.
    pub kind: &'static str,
    pub title: String,
    /// What happened.
    pub message: String,
    /// What to do now.
    pub next_step: String,
    /// Specific plain reasons, for example why each clip was skipped (at most 8).
    pub reasons: Vec<String>,
}

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
    Incompatible { path: PathBuf, reasons: Vec<Reason> },

    /// No file in a reference folder matched.
    #[error(
        "no compatible reference clip found in {}.\n{}",
        .dir.display(),
        .tried.iter().map(|(p, r)| format!("  - {}: {r}", p.display())).collect::<Vec<_>>().join("\n")
    )]
    NoReference {
        dir: PathBuf,
        tried: Vec<(PathBuf, Reason)>,
    },

    /// The `.DAT` is empty, or holds only zero bytes.
    #[error(
        "{} holds no recording data ({detail}): the data isn't in this file. The card's file \
         table points at empty space. Recover the memory card with PhotoRec first (free, \
         https://www.cgsecurity.org), without writing anything to the card, then run datfix \
         on the .DAT or .MP4 pieces it finds.",
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
        /// The recording in plain words, for example `Canon EOS R5 Mark II, 4K 119.88p`.
        camera: String,
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
        camera: String,
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

    /// The error in plain words: title, message, next step. The technical text stays in
    /// `to_string()`.
    pub fn user_message(&self) -> UserMessage {
        let msg = |kind, title: &str, message: String, next: &str| UserMessage {
            kind,
            title: title.into(),
            message,
            next_step: next.into(),
            reasons: Vec::new(),
        };
        match self {
            Error::Io { source, .. } => io_message(source),
            Error::NotADat(_) => msg(
                "not_a_dat",
                "This file can't be repaired",
                "It doesn't contain the recovery index that Canon cameras write into a .DAT \
                 file while they record."
                    .into(),
                "Check that it is the .DAT file from a Canon camera card.",
            ),
            Error::BadReference { path, .. } => msg(
                "bad_reference",
                "The healthy clip can't be read",
                format!(
                    "{} can't be opened as a video. It may be damaged or still copying.",
                    file_name(path)
                ),
                "Choose a clip that plays normally, recorded on the same camera.",
            ),
            Error::Incompatible { path, reasons } => UserMessage {
                reasons: reasons.iter().map(|r| r.user.clone()).collect(),
                ..msg(
                    "incompatible_reference",
                    "The clip doesn't match this .DAT",
                    format!(
                        "{} was recorded with different settings than this .DAT.",
                        file_name(path)
                    ),
                    "Choose a clip recorded on the same camera with the same resolution, frame \
                     rate and sound settings. Or repair without a clip: DATfix then uses its \
                     built-in settings.",
                )
            },
            Error::NoReference { dir, tried } => UserMessage {
                reasons: tried
                    .iter()
                    .take(8)
                    .map(|(p, r)| format!("{}: {}", file_name(p), r.user))
                    .collect(),
                ..msg(
                    "no_reference_found",
                    "No matching clip found",
                    if tried.is_empty() {
                        format!("There are no .MP4 clips in {}.", dir.display())
                    } else {
                        format!(
                            "None of the {} clips checked in {} was recorded with the same \
                             settings as this .DAT.",
                            tried.len(),
                            dir.display()
                        )
                    },
                    "Choose a clip recorded on the same camera with the same resolution, frame \
                     rate and sound settings.",
                )
            },
            Error::EmptyDat { path, detail } => msg(
                "empty_dat",
                "This .DAT file is empty",
                format!(
                    "{} holds no recording data ({detail}). The camera created the file, but \
                     the video was never saved into it, so there is nothing to rebuild here.",
                    file_name(path)
                ),
                "The video may still be on the card. Stop using the card and do not format \
                 it. Recover the card with PhotoRec (free, https://www.cgsecurity.org) without \
                 writing anything to it, then open the .DAT or .MP4 files it finds in DATfix.",
            ),
            Error::NotCanon(what) => msg(
                "not_canon",
                "This isn't a Canon camera .DAT",
                format!("This file is not a recording from a Canon camera: {what}"),
                "DATfix only repairs the .DAT files that Canon cameras leave when a recording \
                 is cut off.",
            ),
            Error::Unsupported(what) => msg(
                "unsupported",
                "This camera's .DAT can't be repaired yet",
                what.clone(),
                &format!(
                    "Keep the .DAT file unchanged. Please share a sample in a GitHub issue \
                     ({ISSUES_URL}) so a later version of DATfix can support this camera."
                ),
            ),
            Error::NoProfile { camera, .. } => msg(
                "no_profile",
                "DATfix needs a healthy clip from this camera",
                format!(
                    "DATfix has no built-in settings for this recording ({camera}). It needs \
                     one clip that plays normally, so it can copy the settings from it."
                ),
                "Choose a clip recorded on the same camera with the same resolution, frame \
                 rate and sound settings. A clip shot just before or after this one is best.",
            ),
            Error::AmbiguousProfile { camera, .. } => msg(
                "ambiguous_profile",
                "DATfix needs a healthy clip to be sure",
                format!(
                    "More than one set of built-in settings fits this recording ({camera}), and \
                     the video alone can't tell which one is right."
                ),
                "Choose a clip that plays normally, recorded on the same camera with the same \
                 settings.",
            ),
            Error::OutputExists(path) => msg(
                "output_exists",
                "A file with that name already exists",
                format!(
                    "{} already exists. DATfix never replaces files.",
                    path.display()
                ),
                "Choose another name or folder for the repaired video.",
            ),
            Error::OutputIsInput => msg(
                "output_is_input",
                "That would overwrite the original",
                "The save location is the .DAT file itself. The original is never changed.".into(),
                "Choose another name for the repaired video.",
            ),
            Error::Cancelled => msg(
                "cancelled",
                "Stopped",
                "The repair was stopped. No partial file was left behind.".into(),
                "Start the repair again when you are ready.",
            ),
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Explains common disk and card errors in plain words.
pub fn io_message(source: &std::io::Error) -> UserMessage {
    use std::io::ErrorKind as K;
    let (kind, title, message, next): (&'static str, &str, &str, &str) = match source.kind() {
        K::NotFound => (
            "not_found",
            "File not found",
            "A file is missing. The card or drive may have been removed.",
            "Reconnect the card or drive, then add the file again.",
        ),
        K::PermissionDenied | K::ReadOnlyFilesystem => (
            "permission_denied",
            "No permission",
            "DATfix is not allowed to read or write here. A locked card or a read-only folder \
             causes this.",
            "Choose another folder to save to. On a Mac, check System Settings > Privacy & \
             Security > Files and Folders.",
        ),
        K::StorageFull => (
            "disk_full",
            "The disk is full",
            "The drive ran out of space while writing. The partial file was removed.",
            "Free up space, or choose another drive to save to.",
        ),
        K::UnexpectedEof => (
            "read_error",
            "The file ended early",
            "The file is shorter than expected. It may still be copying.",
            "Wait until the copy is finished, then try again.",
        ),
        _ if source.raw_os_error() == Some(5) => (
            "read_error",
            "The card or disk reported a read error",
            "Part of the file could not be read from the card or disk.",
            "Copy the .DAT to your computer's internal drive first (a card recovery tool can \
             help), then repair the copy.",
        ),
        _ => (
            "io",
            "A disk error stopped the job",
            "The operating system reported an error while reading or writing.",
            "Check that the card or drive is still connected and has free space, then try again.",
        ),
    };
    UserMessage {
        kind,
        title: title.into(),
        message: message.into(),
        next_step: next.into(),
        reasons: Vec::new(),
    }
}
