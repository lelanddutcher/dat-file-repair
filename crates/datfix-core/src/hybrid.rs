//! Hybrid references: the video track of one clip plus synthesized tracks for the rest.
//!
//! Video decoder settings do not depend on the audio mode. A healthy clip whose *video*
//! matches the `.DAT` (codec, resolution, frame rate, chroma, bit depth, NAL length size) can
//! therefore supply the video track even when its audio, timecode or metadata tracks differ.
//! The missing tracks are built by [`crate::mp4::synth`]. Tracks of the clip that already fit
//! (same kind, count and sample size) are reused as they are.

use crate::error::Reason;
use crate::layout::{DatLayout, Family, FrameRate};
use crate::mp4::atom::Atom;
use crate::mp4::reference::{RefTrack, Reference, TrackKind};
use crate::mp4::synth::{self, PcmKind};
use crate::profile;
use crate::read_at::ReadAt;
use crate::repi::DatIndex;

/// A reference assembled from a clip's video track and synthesized tracks.
pub struct Hybrid {
    pub reference: Reference,
    /// What was synthesized, for the user.
    pub synthesized: Vec<String>,
    /// What was reused from the clip besides the video.
    pub reused: Vec<String>,
    /// What was rebuilt and why, one plain sentence each (no format names).
    pub rebuilt: Vec<String>,
}

fn renumbered(base: &Reference, t: &RefTrack, id: u32) -> Result<Atom, String> {
    let mut trak = base.trak(t).clone();
    synth::set_track_id(&mut trak, id)?;
    Ok(trak)
}

fn internal(e: String) -> Reason {
    Reason::engine(
        "Its settings could not be combined with this .DAT's sound and data tracks.",
        e,
    )
}

/// Channels of a raw AAC frame (no ADTS header) from its first syntax element: a single
/// channel element means mono, a channel pair element stereo.
pub fn aac_channels(first_byte: u8) -> Option<u16> {
    match first_byte >> 5 {
        0 => Some(1),
        1 => Some(2),
        _ => None,
    }
}

/// An AAC track for a `.DAT` whose clip has none: the built-in template of the same body
/// when the bank has one, else one built from the first AAC frame (AAC-LC, 48 kHz, the
/// channel count read from the frame).
fn aac_template<R: ReadAt + ?Sized>(
    src: &R,
    index: &DatIndex,
    layout: &DatLayout,
    id: u32,
) -> Result<(Atom, String), Reason> {
    let code = layout.body_code.map(|c| crate::layout::hex(&c));
    let from_bank = profile::bank()
        .iter()
        .filter(|p| code.is_some() && p.body_code == code)
        .filter_map(|p| p.to_reference().ok().map(|r| (p, r)))
        .find_map(|(p, r)| {
            let t = r
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::CompressedAudio && t.codec.0 == *b"mp4a")?;
            let trak = renumbered(&r, t, id).ok()?;
            Some((
                trak,
                format!("AAC audio (settings of built-in profile {})", p.id),
            ))
        });
    if let Some(found) = from_bank {
        return Ok(found);
    }
    let no_aac = |detail: String| {
        Reason::new(
            "This .DAT has compressed (AAC) sound, and the clip has no compressed sound to copy \
             its settings from.",
            format!(
                "the .DAT has AAC audio but the clip has no AAC track, no built-in profile of \
                 this body has one, and {detail}"
            ),
        )
    };
    let group = index
        .groups
        .iter()
        .find(|g| g.kind.0 == *b"aac " && g.complete > 0)
        .ok_or_else(|| no_aac("no complete AAC frame was found".into()))?;
    let (off, size) = group.samples(1).next().expect("one complete sample");
    let mut b = [0u8; 1];
    if size == 0 || src.read_exact_at(&mut b, off).is_err() {
        return Err(no_aac(format!(
            "the first AAC frame (byte {off}) cannot be read"
        )));
    }
    let channels = aac_channels(b[0]).ok_or_else(|| {
        no_aac(format!(
            "the first AAC frame (byte {off}) does not start with an audio channel element"
        ))
    })?;
    // AAC frames hold 1024 samples. At 48 kHz an interval of F video frames holds
    // F / fps * 48000 / 1024 of them (23-24 per 0.5005 s on Canon bodies).
    let per_interval = layout.shape(b"aac ").map_or(0, |s| s.typical_samples);
    if let (Some(rate), true) = (layout.frame_rate, per_interval > 0) {
        let expected = layout.frames_per_group as f64 / rate.fps() * 48_000.0 / 1024.0;
        if (per_interval as f64 - expected).abs() > 1.5 {
            return Err(no_aac(format!(
                "{per_interval} AAC frames per interval do not fit 48 kHz (expected about \
                 {expected:.1})"
            )));
        }
    }
    let bitrate = 128_000 * channels as u32;
    let trak = synth::aac_trak(id, channels, 48_000, bitrate)
        .ok_or_else(|| no_aac("no AAC settings could be built".into()))?;
    Ok((
        trak,
        format!(
            "AAC-LC 48 kHz {} audio (read from the first frame)",
            if channels == 1 { "mono" } else { "stereo" }
        ),
    ))
}

/// Builds a hybrid reference for `index` from `base`'s video track. `subject` names the
/// source in the plain sentences, for example `your reference clip`.
pub fn assemble<R: ReadAt + ?Sized>(
    src: &R,
    base: &Reference,
    index: &DatIndex,
    layout: &DatLayout,
    subject: &str,
) -> Result<Hybrid, Reason> {
    let shapes = index.shapes();
    let has = |k: &[u8; 4]| shapes.iter().find(|s| s.kind.0 == *k);
    let vt = base
        .video_track()
        .ok_or_else(|| Reason::new("The clip has no video.", "the clip has no video track"))?;
    let rate = FrameRate {
        timescale: vt.timescale,
        frame_duration: vt.sample_delta,
    };
    let per_chunk = layout.frames_per_group.max(1);
    let mut synthesized = Vec::new();
    let mut reused = Vec::new();
    let mut rebuilt = Vec::new();
    let mut traks = vec![renumbered(base, vt, 1).map_err(internal)?];
    let mut next = 2u32;
    let of_kind =
        |k: TrackKind| -> Vec<&RefTrack> { base.tracks.iter().filter(|t| t.kind == k).collect() };

    if let Some(pcm) = has(b"lpcm") {
        let kind = if layout.pcm_frame_bytes == 3 {
            PcmKind::Ipcm24
        } else {
            PcmKind::Twos
        };
        let want = pcm.streams.max(1) as usize;
        let theirs = of_kind(TrackKind::PcmAudio);
        let fits = theirs.len() == want
            && theirs
                .iter()
                .all(|t| t.fixed_sample_size == kind.frame_bytes());
        if fits {
            for t in theirs {
                traks.push(renumbered(base, t, next).map_err(internal)?);
                next += 1;
            }
            reused.push(format!("{want} PCM audio track(s)"));
        } else {
            let why = if !of_kind(TrackKind::CompressedAudio).is_empty() {
                "used compressed (AAC) audio".to_string()
            } else if theirs.is_empty() {
                "had no sound".to_string()
            } else {
                format!(
                    "recorded sound differently ({} track(s) instead of {want})",
                    theirs.len()
                )
            };
            rebuilt.push(format!(
                "Sound settings were rebuilt because {subject} {why}."
            ));
            for _ in 0..want {
                traks.push(synth::pcm_trak(next, kind));
                next += 1;
            }
            synthesized.push(format!("{want} x {} 48 kHz audio", kind.codec()));
        }
    }
    if has(b"aac ").is_some() {
        match of_kind(TrackKind::CompressedAudio).first() {
            Some(t) => {
                traks.push(renumbered(base, t, next).map_err(internal)?);
                reused.push("AAC audio track".into());
            }
            None => {
                let (trak, what) = aac_template(src, index, layout, next)?;
                traks.push(trak);
                synthesized.push(what);
                rebuilt.push(format!(
                    "Sound settings were rebuilt because {subject} had no compressed (AAC) sound."
                ));
            }
        }
        next += 1;
    }
    if has(b"tmcd").is_some() {
        match of_kind(TrackKind::Timecode).first() {
            Some(t) if t.timescale == rate.timescale && t.sample_delta == rate.frame_duration => {
                traks.push(renumbered(base, t, next).map_err(internal)?);
                reused.push("timecode track".into());
            }
            other => {
                traks.push(synth::tmcd_trak(next, rate, per_chunk));
                synthesized.push(format!("tmcd timecode ({rate})"));
                rebuilt.push(format!(
                    "Timecode settings were rebuilt because {subject} {}.",
                    if other.is_some() {
                        "used another timecode rate"
                    } else {
                        "had no timecode"
                    }
                ));
            }
        }
        next += 1;
    }
    if let Some(meta) = has(b"tdmt") {
        let cndm = base
            .tracks
            .iter()
            .find(|t| t.codec.0 == *b"cndm" && t.fixed_sample_size == meta.fixed_size);
        match cndm {
            Some(t) => {
                traks.push(renumbered(base, t, next).map_err(internal)?);
                reused.push("Canon metadata track".into());
            }
            None => {
                traks.push(synth::cndm_trak(next, 1, rate, meta.fixed_size, per_chunk));
                synthesized.push("cndm Canon metadata".into());
                rebuilt.push(format!(
                    "The camera's per-frame data track was rebuilt because {subject} stored it \
                     differently."
                ));
            }
        }
        next += 1;
    }

    // File type: the clip's own when it is the same family, else Canon's for the family.
    let brand = layout.family.brand();
    let ftyp = if layout.family != Family::Unknown && !base.ftyp.windows(4).any(|w| w == brand.0) {
        synthesized.push(format!("ftyp ({brand} brand)"));
        synth::ftyp(layout.family, vt.codec)
    } else {
        base.ftyp.clone()
    };

    let model = base
        .moov
        .child(b"udta")
        .and_then(synth::model_of)
        .or_else(|| layout.body.map(str::to_string));
    let mut children = vec![synth::mvhd(base.movie_timescale, next)];
    children.extend(traks);
    if let Some(m) = model {
        children.push(synth::udta_model(&m));
    }
    let moov = Atom::container(b"moov", children);
    let mut reference =
        Reference::from_parts(&base.path, base.file_size, ftyp, moov).map_err(internal)?;
    // The synthesized tracks follow the .DAT's own layout by construction; the video layout was
    // compared with the clip before assembling.
    reference.repi_shapes = Some(shapes);
    Ok(Hybrid {
        reference,
        synthesized,
        reused,
        rebuilt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Family, StartTime};
    use crate::repi::DatIndex;

    #[test]
    fn aac_channels_from_the_first_element() {
        assert_eq!(aac_channels(0x00), Some(1));
        assert_eq!(aac_channels(0x21), Some(2));
        assert_eq!(aac_channels(0xe0), None); // END
        assert_eq!(aac_channels(0xc0), None); // FIL
    }

    #[test]
    fn aac_template_comes_from_the_bank_for_a_known_body() {
        let layout = DatLayout {
            family: Family::BPrime,
            frames_per_group: 30,
            frame_rate: None,
            frame_rate_from_audio: false,
            body_code: Some([0x00, 0x10, 0x42]), // EOS R5
            body: Some("Canon EOS R5"),
            codec: None,
            long_gop: None,
            has_records: true,
            pcm_streams: 0,
            pcm_frame_bytes: 0,
            shapes: Vec::new(),
            start: StartTime::default(),
        };
        let index = DatIndex::default();
        let (trak, what) = aac_template(&[][..], &index, &layout, 3).unwrap();
        assert!(what.contains("built-in profile R5-"), "{what}");
        let mut probe = trak.clone();
        synth::set_track_id(&mut probe, 3).unwrap();
        assert_eq!(probe, trak);
        let bytes = trak.to_bytes();
        assert!(bytes.windows(4).any(|w| w == b"mp4a"));
        // An unknown body and no AAC frame: the clear error.
        let unknown = DatLayout {
            body_code: Some([9, 9, 9]),
            ..layout
        };
        let err = aac_template(&[][..], &index, &unknown, 3).unwrap_err();
        assert!(
            err.detail.contains("no complete AAC frame"),
            "{}",
            err.detail
        );
    }
}
