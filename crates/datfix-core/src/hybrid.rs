//! Hybrid references: the video track of one clip plus synthesized tracks for the rest.
//!
//! Video decoder settings do not depend on the audio mode. A healthy clip whose *video*
//! matches the `.DAT` (codec, resolution, frame rate, chroma, bit depth, NAL length size) can
//! therefore supply the video track even when its audio, timecode or metadata tracks differ.
//! The missing tracks are built by [`crate::mp4::synth`]. Tracks of the clip that already fit
//! (same kind, count and sample size) are reused as they are.

use crate::layout::{DatLayout, Family, FrameRate};
use crate::mp4::atom::Atom;
use crate::mp4::reference::{RefTrack, Reference, TrackKind};
use crate::mp4::synth::{self, PcmKind};
use crate::repi::DatIndex;

/// A reference assembled from a clip's video track and synthesized tracks.
pub struct Hybrid {
    pub reference: Reference,
    /// What was synthesized, for the user.
    pub synthesized: Vec<String>,
    /// What was reused from the clip besides the video.
    pub reused: Vec<String>,
}

fn renumbered(base: &Reference, t: &RefTrack, id: u32) -> Result<Atom, String> {
    let mut trak = base.trak(t).clone();
    synth::set_track_id(&mut trak, id)?;
    Ok(trak)
}

/// Builds a hybrid reference for `index` from `base`'s video track.
pub fn assemble(base: &Reference, index: &DatIndex, layout: &DatLayout) -> Result<Hybrid, String> {
    let shapes = index.shapes();
    let has = |k: &[u8; 4]| shapes.iter().find(|s| s.kind.0 == *k);
    let vt = base.video_track().ok_or("the clip has no video track")?;
    let rate = FrameRate {
        timescale: vt.timescale,
        frame_duration: vt.sample_delta,
    };
    let per_chunk = layout.frames_per_group.max(1);
    let mut synthesized = Vec::new();
    let mut reused = Vec::new();
    let mut traks = vec![renumbered(base, vt, 1)?];
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
                traks.push(renumbered(base, t, next)?);
                next += 1;
            }
            reused.push(format!("{want} PCM audio track(s)"));
        } else {
            for _ in 0..want {
                traks.push(synth::pcm_trak(next, kind));
                next += 1;
            }
            synthesized.push(format!("{want} x {} 48 kHz audio", kind.codec()));
        }
    }
    if has(b"aac ").is_some() {
        let theirs = of_kind(TrackKind::CompressedAudio);
        let t = theirs.first().ok_or(
            "the .DAT has AAC audio but the clip has no AAC track, and AAC settings cannot be \
             synthesized",
        )?;
        traks.push(renumbered(base, t, next)?);
        next += 1;
        reused.push("AAC audio track".into());
    }
    if has(b"tmcd").is_some() {
        match of_kind(TrackKind::Timecode).first() {
            Some(t) if t.timescale == rate.timescale && t.sample_delta == rate.frame_duration => {
                traks.push(renumbered(base, t, next)?);
                reused.push("timecode track".into());
            }
            _ => {
                traks.push(synth::tmcd_trak(next, rate, per_chunk));
                synthesized.push(format!("tmcd timecode ({rate})"));
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
                traks.push(renumbered(base, t, next)?);
                reused.push("Canon metadata track".into());
            }
            None => {
                traks.push(synth::cndm_trak(next, 1, rate, meta.fixed_size, per_chunk));
                synthesized.push("cndm Canon metadata".into());
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
    let mut reference = Reference::from_parts(&base.path, base.file_size, ftyp, moov)?;
    // The synthesized tracks follow the .DAT's own layout by construction; the video layout was
    // compared with the clip before assembling.
    reference.repi_shapes = Some(shapes);
    Ok(Hybrid {
        reference,
        synthesized,
        reused,
    })
}
