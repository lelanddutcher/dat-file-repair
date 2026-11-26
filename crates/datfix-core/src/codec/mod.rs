//! Video codec helpers: find the slice header inside each frame and read what the sample
//! tables need (keyframe flag and display order).
//!
//! Each video sample in a Canon `.DAT` is one frame stored as length-prefixed NAL units
//! (usually a 4-byte big-endian length, then the NAL unit). Parameter sets are not repeated in
//! the stream; they come from the reference clip.

pub mod avc;
pub mod bits;
pub mod hevc;

use crate::fourcc::FourCC;
use crate::read_at::ReadAt;

/// How many bytes of a slice NAL unit to read for header parsing.
const SLICE_HEADER_BYTES: usize = 96;

/// Codec configuration taken from the reference's sample entry.
#[derive(Clone, Debug)]
pub enum VideoCodec {
    Hevc(hevc::HevcConfig),
    Avc(avc::AvcConfig),
}

impl VideoCodec {
    /// Builds the codec from a sample entry type and its `hvcC`/`avcC` payload.
    pub fn from_config(
        codec: FourCC,
        config_kind: Option<FourCC>,
        config: &[u8],
    ) -> Result<Self, String> {
        match (&codec.0, config_kind.map(|k| k.0)) {
            (b"hvc1" | b"hev1", Some(k)) if k == *b"hvcC" => {
                hevc::HevcConfig::from_hvcc(config).map(VideoCodec::Hevc)
            }
            (b"avc1" | b"avc3", Some(k)) if k == *b"avcC" => {
                avc::AvcConfig::from_avcc(config).map(VideoCodec::Avc)
            }
            _ => Err(format!(
                "video codec {codec} is not supported (HEVC and H.264 are)"
            )),
        }
    }

    /// Bytes in each NAL length prefix.
    pub fn length_size(&self) -> u8 {
        match self {
            VideoCodec::Hevc(c) => c.length_size,
            VideoCodec::Avc(c) => c.length_size,
        }
    }

    /// Short human name.
    pub fn name(&self) -> &'static str {
        match self {
            VideoCodec::Hevc(_) => "HEVC",
            VideoCodec::Avc(_) => "H.264",
        }
    }

    /// Picture size from the first SPS.
    pub fn picture_size(&self) -> (u32, u32) {
        match self {
            VideoCodec::Hevc(c) => (c.sps[0].width, c.sps[0].height),
            VideoCodec::Avc(c) => (c.sps[0].width, c.sps[0].height),
        }
    }
}

/// Facts about one video frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInfo {
    /// Random access point (keyframe).
    pub sync: bool,
    /// `(sequence, poc)`: sorting by this gives display order.
    pub order: (u32, i64),
}

/// Reads frames one after another in decode order and tracks POC state between them.
pub struct FrameAnalyzer {
    codec: VideoCodec,
    hevc_poc: hevc::PocTracker,
    avc_poc: avc::PocTracker,
}

impl FrameAnalyzer {
    pub fn new(codec: VideoCodec) -> Self {
        FrameAnalyzer {
            codec,
            hevc_poc: hevc::PocTracker::default(),
            avc_poc: avc::PocTracker::default(),
        }
    }

    /// Analyzes the frame stored at `offset` with `size` bytes.
    ///
    /// Walks the NAL units of the frame, skips non-slice units (AUD, SEI, ...) and parses the
    /// first slice header. Fails when the NAL lengths do not add up to the sample size, which
    /// means the frame (or the `repi` size table) is damaged.
    pub fn analyze<R: ReadAt + ?Sized>(
        &mut self,
        src: &R,
        offset: u64,
        size: u32,
    ) -> Result<FrameInfo, String> {
        let ls = self.codec.length_size() as u64;
        let end = offset + size as u64;
        let mut pos = offset;
        while pos + ls < end {
            let mut head = [0u8; 6];
            let n = (ls as usize + 2).min((end - pos) as usize);
            src.read_exact_at(&mut head[..n], pos)
                .map_err(|e| e.to_string())?;
            let nal_len = head[..ls as usize]
                .iter()
                .fold(0u64, |a, &b| (a << 8) | b as u64);
            let nal_start = pos + ls;
            if nal_len == 0 || nal_start + nal_len > end {
                return Err(format!(
                    "NAL unit length {nal_len} at byte {} does not fit the frame",
                    pos - offset
                ));
            }
            let header0 = head[ls as usize];
            let is_slice = match self.codec {
                VideoCodec::Hevc(_) => hevc::is_vcl(hevc::nal_type(header0)),
                VideoCodec::Avc(_) => avc::is_vcl(avc::nal_type(header0)),
            };
            if is_slice {
                let take = (nal_len as usize).min(SLICE_HEADER_BYTES);
                let nal = src.read_vec(nal_start, take).map_err(|e| e.to_string())?;
                return self.parse_slice(&nal);
            }
            pos = nal_start + nal_len;
        }
        Err("frame contains no slice".into())
    }

    fn parse_slice(&mut self, nal: &[u8]) -> Result<FrameInfo, String> {
        match &self.codec {
            VideoCodec::Hevc(cfg) => {
                let s = hevc::parse_slice(nal, cfg)?;
                Ok(FrameInfo {
                    sync: hevc::is_irap(s.nal_type),
                    order: self.hevc_poc.next(&s),
                })
            }
            VideoCodec::Avc(cfg) => {
                let s = avc::parse_slice(nal, cfg)?;
                Ok(FrameInfo {
                    sync: s.nal_type == 5,
                    order: self.avc_poc.next(&s)?,
                })
            }
        }
    }
}
