# DATfix — plan

Status (2026-11-26, end of day): **M0–M4 done. M5 (first public release) is next.**

- Engine + CLI: all 36 corpus `.DAT`s (R3, R5, R5 II) repair in both modes (folder clip, built-in
  settings) with full clean decodes (72/72). Fuzzed; failing-card, FAT32, disk-full and Windows
  read-error paths handled. Reference metadata is not copied (privacy). Dates, rotation, AAC hybrids.
- Card recovery: `datfix scan` and the app's "Recover from a memory card" find deleted and 0-byte
  `.DAT`s in a card image, rejoin split recordings, restore original names and times.
- App: Tauri, macOS universal `.dmg` + Windows installers built in CI. Verified in QuickTime and Resolve.
- Release: `release.yml` builds all README assets and drafts a GitHub Release on a `v*` tag.
- Open (see `docs/backlog/next.md`): real-Windows run, signing/notarization, admin-password card
  imaging, Premiere test, card time-zone check on a real Canon card.
- Name DATfix; repo `lelanddutcher/dat-file-repair`. License MIT.

A general video-repair tool is planned as a separate project.

## Goal

A free, open-source desktop app (macOS + Windows) that turns a Canon camera `.DAT` file back into a
playable `.MP4`. The user drops in the broken `.DAT` and one healthy clip from the same camera. The
app writes a new `.MP4`. It never modifies the original.

## What a Canon `.DAT` is (verified on a test corpus, 2026-11-26)

A Canon camera writes the media payload to a temporary `.DAT` while it records. It writes the file
header (`ftyp` + `moov`, the index that tells a player where each frame is) only when recording
stops. If the camera loses power or the card errors, the header never gets written, and the `.DAT`
is left on the card. Players cannot open it.

What we found in 37 Canon `.DAT` files in a local test corpus:

- Every file starts with an `mdat` box whose size is still `0` (never finalized).
- The payload is cut into groups. Each group starts with a Canon **`repi`** header that lists the
  track type, the sample count and the exact byte size of every sample.
- **R5 Mark II (brand `XFVC`, XF-HEVC S):** `repi` covers every track — `vide` (HEVC 4:2:2 10-bit),
  four `lpcm` groups (one per mono 24-bit channel), `tmcd` (timecode) and `tdmt` (Canon metadata).
  Pattern per 0.5 s: `tmcd ×60, tdmt ×60, lpcm ×24024 (×4), vide ×60`.
- **R3 (brand `CAEP`):** `repi` covers `vide`; small (~1.97 KB) gaps between groups are probably
  audio or padding. **Not yet decoded.**
- The video has no in-band parameter sets (VPS/SPS/PPS). They live only in the `moov` of a finished
  file, so a healthy reference clip with the same codec settings is required.

Proof: from a 64 MB R5 II `.DAT`, `tools/repi_extract_hevc.py` + parameter sets from an unrelated
R5 II clip decoded 135/135 frames with zero decoder errors.

## Why existing tools fail

- `untrunc` (anthwlock fork) added a `repi` parser on 2026-11-01, but only for brand `CAEP` with
  HEVC + AAC + timecode. It refuses `XFVC` (R5 II), PCM audio, H.264, and extra tracks like `cndm`.
  Everything else falls back to guessing sample boundaries, which breaks on PCM audio.
- ffmpeg cannot open a file without `moov`.
- Commercial tools (Grau, Repairit, Stellar, Fixo) cost $30–120 and have no R5 II/R3 success reports.
  Services (Treasured, MP4repair.org, RichardRecovery) charge from $49 per clip to $8/min.
  Full survey: `docs/research/existing-tools.md`.

The gap: **nobody walks the `repi` index for every track and every Canon brand.** With the index,
repair is deterministic — no guessing.

## Repair algorithm

1. **Reference parse.** Read the healthy clip: `ftyp`, Canon `uuid`, every `trak`'s `stsd` entry
   (copied byte for byte), timescales, `mdat` start offset.
2. **Compatibility check.** Codec, resolution, frame rate, chroma, bit depth, audio format and channel
   count must match the `repi` stream. Refuse with a clear message if not.
3. **Index walk.** Walk `repi` groups from the `.DAT` start. Validate each header strictly
   (`header_len == 20 + 4n`, flag `0x83`, known type). Record every sample's offset and size. Stop at
   the first incomplete group; drop the partial tail.
4. **Fallback scan** (only when `repi` is missing or damaged mid-file): resynchronize on the next
   valid `repi`; for regions with none, use NAL-unit walking for video.
5. **Tables.** Build `stsz`, `co64`, `stsc`, `stts` (constant duration), `stss` (IRAP NAL types
   16–21), `ctts` (from slice-header POC for Long-GOP; none for intra).
6. **Write.** New file = `ftyp` + rebuilt `moov` + `mdat` header, then the `.DAT` bytes copied
   unchanged. Never write into the `.DAT`.
7. **Verify.** `ffprobe` frame count, keyframes, PTS order; full decode of the last GOP; audio vs
   video duration within one frame.

## Product shape

- **Core:** Rust library + CLI (`datfix repair broken.DAT --reference good.MP4 -o fixed.MP4`).
  Box writing via the `mp4-atom` crate (supports `hvc1`, `avc1`, `ipcm`/`lpcm`/`twos`, `co64`,
  unknown boxes as raw bytes).
- **App:** Tauri (Rust backend, small web UI) — one window, two drop zones, one button, a progress
  bar, a result preview. Builds for macOS (universal) and Windows. Chosen over Electron for size
  (~10 MB vs ~150 MB) and because the core is Rust anyway.
- **License:** MIT. (untrunc is GPL-2.0; DATfix is its own code and cites it.)

## Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 | Research + corpus | Survey written; all 37 `.DAT`s catalogued with camera, brand, track pattern, a matched reference. |
| M1 | R5 II end to end (CLI) | 64 MB + one multi-GB R5 II `.DAT` → MP4 that plays in QuickTime, Resolve, Premiere with all 4 audio channels in sync. |
| M2 | R3 + older bodies | R3 `CAEP` files (H.264 and HEVC, AAC and PCM) repaired; R5/R6-era `100CANON` files tested. |
| M3 | Robustness | Truncated tails, damaged `repi` mid-file, >4 GB, Long-GOP `ctts`. Regression suite against the corpus. |
| M4 | Desktop app | Tauri app, signed builds for macOS + Windows. |
| M5 | Public release | Repo public, signed builds attached to a GitHub Release. |

## Workspace

- Media lives outside the repo, in a local workspace (`$DATFIX_CORPUS`, `$DATFIX_SAMPLES`).
  Test copies are read only.
- Originals are never written to. We copy, then work on the copy.
- Scripts live in this repo under `tools/`; media never enters git.
