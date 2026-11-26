# Rust learnings (macOS)

A practical log for agents who port the owner's tools to Rust on macOS. It is based on
building `datfix`, the Canon `.DAT` repair core and CLI. Short notes, most useful first.

## Toolchain

- Rust comes from `rustup`. It installs `cargo`, `rustc`, `clippy` and `rustfmt` in
  `~/.cargo/bin`. Check with `cargo --version` and `rustup component list --installed`.
- This repo was built with Rust 1.94 (stable). The workspace sets `rust-version = "1.85"`,
  the first version with the 2024 edition.
- No Xcode project is needed. The Xcode command-line tools provide the linker.
- `int_roundings` (for example `i64::div_ceil`) is still unstable for signed integers.
  `u64::div_ceil` is stable. Clippy suggests `div_ceil` for unsigned code; use it there.

## Workspace layout

- Root `Cargo.toml` is a workspace only. It lists the members and shares the version,
  edition, license and dependency versions (`[workspace.package]`, `[workspace.dependencies]`).
- `crates/datfix-core` is the library. All logic lives here.
- `crates/datfix` is the CLI binary. It only parses arguments and prints. A GUI (Tauri) will be
  a third member that depends on the core, so keep the CLI thin.
- Commit `Cargo.lock`. The workspace builds a binary, and the lock file makes builds repeatable.
- `target/` is ignored by `.gitignore`.

## Crates chosen and why

| Crate | Why |
|---|---|
| `clap` (derive) | Standard CLI parser. `conflicts_with` and `required_unless_present` express "either `--reference` or `--reference-dir`". |
| `serde` + `serde_json` | Progress events and results become JSON lines with `#[serde(tag = "event")]`. One enum serves both the CLI and a future GUI. |
| `thiserror` | Gives each error a friendly `Display` message without boilerplate. |
| `tempfile` (dev only) | Temporary folders in tests. |

Not used, on purpose:

- **No MP4 crate** (`mp4-atom`, `mp4`). The repair copies almost every box verbatim and rebuilds
  only a few tables. A 200-line box tree (`mp4/atom.rs`) was simpler than mapping a full typed
  model, and it can never drop an unknown Canon box by accident.
- **No bit-reader or HEVC crate.** The slice-header fields we need fit in ~300 lines and are easy
  to test.
- **No `anyhow`** in the core. Library errors are a typed enum so a GUI can match on them.

## Reading huge files

- Use positional reads, not a shared cursor. `std::os::unix::fs::FileExt::read_at` on macOS and
  Linux, `std::os::windows::fs::FileExt::seek_read` on Windows. Wrap both behind one trait
  (`read_at.rs`) and implement the same trait for `[u8]` so unit tests run on in-memory data.
- `read_at` may return fewer bytes than asked. Loop until the buffer is full (`read_exact_at`).
- Scanning for a magic value: probe the expected position first with a 4-byte read. Only fall
  back to block scanning when the probe fails. A naive 1 MiB read per group multiplied the I/O
  for small audio groups.
- Copy large payloads with an 8 MiB buffer and `write_all`. An 80 GB copy is limited by the disk,
  not the code.
- Open inputs with `File::open` only (read-only). Create outputs with
  `File::options().write(true).create_new(true)` so nothing is ever clobbered. Write to a hidden
  temp file next to the output and `rename` on success; delete the temp file on failure.
- Call `sync_all()` before the rename.

## Testing against large media

- Media never goes into git. Synthetic data covers the logic; real files cover reality.
- Unit tests sit next to the code (`#[cfg(test)] mod tests`). They build tiny `repi` streams in
  memory: clean, padded, truncated, damaged header, misaligned samples.
- Integration tests live in `crates/datfix-core/tests/`. Shared builders go in
  `tests/common/mod.rs` (a folder, so Cargo does not treat it as its own test).
- Tests that need real media are gated on environment variables and return early when unset,
  so `cargo test` stays fast and green everywhere:
  - `DATFIX_REF=<healthy clip>`: compares our `stss`/`ctts`/edit shift with the camera's tables.
  - `DATFIX_CORPUS=<folder>`: repairs every `.DAT` with references from the same folder.
- Run media tests with `--release`. Debug builds are 10–50× slower at byte crunching.
- Use `-- --nocapture` to see `eprintln!` progress from tests.
- `cargo test --release` builds the test binaries but **does not rebuild the CLI binary**. Run
  `cargo build --release` before using `target/release/<bin>` by hand, or you test stale code.
- Verify outputs with ffmpeg tools:
  - `ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 out.mp4`
  - `ffmpeg -v error -i out.mp4 -f null -` must print nothing.
  - `ffprobe -show_entries stream=index,codec_type,duration_ts,time_base` for durations.
  - Extract frames with `ffmpeg -ss <t> -i out.mp4 -frames:v 1 -vf scale=1024:-1 x.jpg` and
    look at them.
- A healthy clip is the best oracle. Running our analysis on the camera's own samples and
  comparing with the camera's own tables caught a keyframe rule difference that synthetic tests
  could not.

## Dev loop

```sh
cargo build                                  # debug build
cargo test                                   # unit + integration tests (media tests skip)
cargo clippy --all-targets -- -D warnings    # lints, warnings are errors
cargo fmt                                    # format; CI runs `cargo fmt --check`
cargo build --release                        # optimized binary in target/release/
```

- Run `cargo fmt` before `clippy`; it rewrites files and saves a round trip.
- Clippy lints met here: `manual_range_contains` (write `(a..=b).contains(&x)`), unused functions
  after refactors (`dead_code` is an error under `-D warnings`).
- A test-helper module used by only some test files needs `#![allow(dead_code)]`.

## MP4 box writing

- A box is `u32 size | 4-byte type | payload`. Size 1 means a 64-bit size follows; size 0 means
  "to end of file" (Canon `.DAT` files start with a size-0 `mdat`).
- Always write `co64` (64-bit chunk offsets). It costs 4 extra bytes per chunk and removes the
  4 GiB limit.
- Write `mdat` with the 16-byte large header so payloads above 4 GiB work.
- `mvhd`, `tkhd` and `mdhd` have version 0 (32-bit times/durations) and version 1 (64-bit).
  Parse both into one struct and re-emit version 1 only when a value no longer fits.
- Build the `moov` twice: once to learn its size, then with the real chunk offsets. The size does
  not depend on offset values because `co64` entries are fixed width.
- `ctts` version 0 offsets cannot be negative. Shift all offsets up by the largest "display
  before decode" distance and put the same shift into the edit list (`elst` media time). Canon
  does exactly this (one frame at 119.88p).
- Drop sample-table boxes you cannot rebuild correctly (`sdtp`, `sgpd`, `sbgp`) instead of
  copying stale ones. Report what was dropped.
- Keep the reference's `stsd` (codec settings) byte for byte. Canon video has no in-band
  parameter sets; the decoder needs `hvcC`/`avcC` from the reference.

## Canon-specific gotchas

- Not all Canon video samples are packed. R3 (`CAEP`) pads each video sample to an 8-byte file
  offset; R5 Mark II (`XFVC`) packs them. Detect it per group: for each candidate alignment,
  check that the NAL length prefixes of **every** sample add up exactly to its size. Checking
  only the first few samples let a wrong alignment pass by chance.
- Padded samples are not contiguous, so each becomes its own chunk (Canon's own R3 files use one
  sample per chunk).
- Older bodies store a PCM group as one big sample (96096 bytes = 24024 stereo 16-bit frames).
  MP4 wants one sample per audio frame; split it to match the reference's `stsz`.
- A finished Canon MP4 still contains the `repi` groups inside its `mdat`. Walking them gives a
  fingerprint of the recording mode (streams, samples per group). Comparing it with the `.DAT`
  rejects references from another frame rate or audio setting.
- Canon lists only IDR frames in `stss`. CRA frames followed by RASL pictures are not listed,
  because those leading pictures cannot be decoded when playback starts at the CRA.
- Files mirrored from the NAS can arrive with mode `070` (no owner read). Reads fail with
  "Permission denied" until the mirror's final `chmod 444` runs.

## Engine v2: repair without a matching reference

Notes from adding the profile bank, hybrid references and damage handling.

### Canon facts learned on the way

- **Padding is on the size, not the offset.** In family B (`CAEP` + LPCM) the video data
  starts at 4 mod 8, so "align each sample to an 8-byte file offset" puts every frame 4 bytes
  off and the NAL check fails. Sample `i + 1` starts `round_up(size_i, 8)` after sample `i`.
  Family A hid the bug because its data happens to start at 0 mod 8.
- **`tmcd` is time of day.** The first timecode counter divided by the frame rate is the
  local time when recording started (it matched the file name `H241221_134042` to the
  second). It is a 32-bit frame count, so it measures lost frames exactly.
- **The record frame counter is 7 bits** (byte `0x54` of each 128-byte record, `& 0x7f`). It
  wraps every 128 frames; combine it with a byte-count estimate to pick the wrap.
- **The `moov` `uuid` box holds a JPEG thumbnail with Exif** (`CNTH`), and `udta/loci` holds
  GPS. Never ship either; the profile bank strips them.
- **Video settings do not depend on the audio mode.** An R5 Long-GOP AAC clip's `hvcC`
  decoded R5 All-Intra LPCM `.DAT` frames cleanly. All Canon 4K 4:2:2 10-bit HEVC bodies in
  the corpus share the same SPS/PPS fields; a full slice-header parse cannot tell R3 from
  R5 Mark II, so the body code from the records is part of the profile key.
- **Profiles that differ only in VUI** (colour tags) or in the `tkhd` matrix (rotation)
  decode identically. Reset the matrix at harvest; group the rest by parsed SPS/PPS fields.

### Techniques

- **Verify settings by parsing to the end of the slice header.** A wrong SPS/PPS almost
  never lands on `byte_alignment()` (a 1 bit, then zeros). With wavefronts (WPP), the entry
  point count must equal CTB rows - 1 for a one-slice picture; slice addresses must be inside
  the picture. Reading 4 KiB per slice NAL is enough for the entry point table.
- **Embed data with `include_str!` + `serde_json`.** The bank is ~40 KB of JSON with hex
  strings; diffs stay reviewable and a unit test checks every entry still parses and holds no
  `uuid`/`loci`/`CNTH`.
- **A virtual offset for inserted data.** Silence chunks point at a zero block appended after
  the copied payload. Give them the `.DAT` offset `copy_end`; the same offset shift as every
  other chunk then lands them on the zero block.
- **Lost video frames: stretch the previous frame's `stts` delta**, do not invent frames.
  Later frames keep their time, the picture holds over the gap.
- **Testing sync: compare two repairs.** Repair a clean copy and a damaged copy; map every
  output sample back to its `.DAT` offset and assert the decode time is the same in both.
  It checks every track at once, with no media knowledge in the test.
- `Result::unwrap_err()` needs `T: Debug`. For results holding big structs, use
  `let Err(e) = ... else { panic!() };` instead of deriving `Debug` everywhere.
- Shell: `scp` with a remote path in escaped quotes fails under the SFTP protocol ("No such
  file"). `ssh host "cat \"$path\"" > file` works.
