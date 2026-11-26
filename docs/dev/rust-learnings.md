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

## Desktop app (Tauri)

### What Tauri is

Tauri builds a desktop app from two parts. A Rust program (the "backend") opens a native window.
The window shows a web page (the "frontend") that the system web view draws: WebKit on macOS,
WebView2 on Windows. No browser engine ships inside the app, so the `.dmg` is about 6 MB.

The page calls Rust functions marked `#[tauri::command]` with `invoke("name", { args })`. Rust
sends progress back through a `tauri::ipc::Channel`, which the page creates and passes as an
argument.

### Layout

| Path | Holds |
|---|---|
| `app/src/` | The page: `main.ts` (state and rendering), `api.ts` (typed command wrappers), `dom.ts` (element builder), `format.ts`, `styles.css`. Plain TypeScript, built by Vite. |
| `app/src-tauri/src/engine.rs` | The only module that calls `datfix-core`. Converts engine results and errors into plain-words types. Fix this file when the engine API changes. |
| `app/src-tauri/src/commands.rs` | The commands. Each moves slow work to `spawn_blocking`, keeps cancel flags and cached `.DAT` indexes. |
| `app/src-tauri/src/lib.rs` | Builds the app, registers plugins and commands, handles window close. |
| `app/src-tauri/tauri.conf.json` | Product name, window, bundle and signing settings. |
| `app/src-tauri/capabilities/default.json` | What the page may call (Tauri v2 denies everything not listed). |
| `app/src-tauri/tests/engine_media.rs` | Real-media tests of the engine layer, gated on environment variables. |

The app's name is `productName` in `tauri.conf.json`. The window title and page heading read it at
start-up, so a rename is a one-line change (plus `identifier` if the bundle id should change).

### Commands

```sh
cd app
npm ci                                   # once; installs Vite, TypeScript and the Tauri JS API
cargo tauri dev                          # runs Vite and the app with hot reload of the page
npm run typecheck                        # TypeScript check
cargo tauri build --bundles app          # quick release .app for this Mac only
cargo tauri build --target universal-apple-darwin   # .app + .dmg for Apple Silicon and Intel
```

- The universal build needs both Rust targets: `rustup target add x86_64-apple-darwin`.
- Outputs land in the workspace `target/`, for example
  `target/universal-apple-darwin/release/bundle/dmg/DATfix_0.1.0_universal.dmg`.
- The app is a workspace member but not a `default-members` entry. Plain `cargo build`, `test`
  and `clippy` at the root skip it, because it needs `app/dist` (the built page) and, on Linux,
  WebKitGTK. Check it with `cargo clippy -p datfix-app --all-targets -- -D warnings` after
  `npm run build`.
- Media tests: `DATFIX_APP_DAT=<.DAT> DATFIX_APP_REFS=<folder> DATFIX_APP_OUT=<folder>
  [DATFIX_APP_BIG_DAT=<multi-GB .DAT>] cargo test --release -p datfix-app -- --nocapture`.

### Gotchas met

- **Versions must match.** `tauri build` refuses to run when the npm packages
  (`@tauri-apps/api`, `plugin-dialog`, `plugin-opener`) are on a different minor version than the
  Rust crates. The workspace `rust-version = "1.85"` makes Cargo pick the newest crate versions
  that support Rust 1.85 (tauri 2.11), which are older than the newest npm packages. Pin the npm
  packages with `~` to the same minor. Check with `cargo tree -p datfix-app -d` and
  `npm ls`.
- **Clippy `result_large_err`.** A rich error struct (title, message, next step, reasons) trips
  this lint. The app allows it at crate level; errors are rare and become JSON.
- **Long work must leave the main thread.** A command that blocks freezes the window. Use
  `async fn` commands plus `tauri::async_runtime::spawn_blocking`.
- **Cancel = shared `AtomicBool`.** The engine checks it about every 8 MB and deletes its
  temporary file. Closing the window during a repair sets every flag and waits (up to 15 s) for
  jobs to finish before the process exits. A forced kill (`kill`, Activity Monitor) still leaves
  a hidden `.<name>.datfix-partial` file next to the output.
- **Drag and drop gives real paths only through Tauri.** HTML5 drop events in the web view do
  not carry file paths. Use `getCurrentWebview().onDragDropEvent`.
- **The web view cannot be clicked by test tools on macOS** (WKWebView has no WebDriver). The app
  reads `DATFIX_DEV_AUTORUN` (JSON: `dats`, `reference`, `outDir`, `repair`, `cancelAfterMs`,
  `scrollY`) and drives its own UI, then prints `DATFIX_DEV_REPORT <json>` to stdout.
  `DATFIX_DEV_THEME=light|dark` forces the theme. Window screenshots: find the window id with
  `CGWindowListCopyWindowInfo` (a 10-line Swift program) and run `screencapture -x -o -l <id>`.

### Signing and notarization (not done yet)

Unsigned builds work, but macOS Gatekeeper warns ("cannot be opened because the developer cannot
be verified") and Windows SmartScreen shows "Windows protected your PC". Before a public release:

macOS:

1. Join the Apple Developer Program (99 USD/year).
2. Create a "Developer ID Application" certificate in Xcode or on developer.apple.com. Export it
   as `.p12` with a password.
3. Create an app-specific password for the Apple ID at appleid.apple.com.
4. Set these environment variables (or GitHub secrets, see `.github/workflows/app.yml`):
   `APPLE_CERTIFICATE` (base64 of the `.p12`), `APPLE_CERTIFICATE_PASSWORD`,
   `APPLE_SIGNING_IDENTITY` (`Developer ID Application: Name (TEAMID)`), `APPLE_ID`,
   `APPLE_PASSWORD` (the app-specific password), `APPLE_TEAM_ID`.
5. `cargo tauri build` then signs with the hardened runtime (already on in `tauri.conf.json`) and
   notarizes. Check with `spctl -a -vv DATfix.app` and `xcrun stapler validate DATfix.dmg`.

Windows:

1. Buy a code-signing certificate (OV or EV), or use Azure Trusted Signing.
2. Put its thumbprint in `bundle.windows.certificateThumbprint` and set a `timestampUrl`
   (for example `http://timestamp.digicert.com`), or configure `bundle.windows.signCommand`
   for Azure Trusted Signing.
3. Install the certificate on the build machine (a CI step imports it from a secret).

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

## App on engine v2

Notes from moving the desktop app onto the reference-free engine.

### API shape that worked for a GUI

- **Two texts per error.** `Error`'s `Display` stays technical (paths, offsets, codec names).
  `Error::user_message()` returns a title, a message, a next step and plain reasons. The CLI
  prints the user message first and the technical text under `details:`; the app shows the
  technical text behind a "Details" disclosure. One source of words keeps the CLI and the app
  saying the same thing.
- **Reasons are pairs.** A clip that does not fit gets a `Reason { user, detail }`. Producing
  both where the check happens is easier than translating jargon afterwards with string
  matching.
- **Search returns verdicts, not just a winner.** `search_references` lists every clip it
  checked (fits, video only, skipped + reason) and the chosen one. The app shows "N clips
  checked" with a reason per clip, and the fallback note ("None of the 4 clips in the folder
  “100CANON” matches this file") comes from the same data.
- **Reuse the index.** `RepairOptions::index` takes the index `inspect_with` returned. A 38 GB
  file's walk takes about 40 s; the app reads it once when the file is added. The engine
  ignores a cached index whose file size no longer matches the file.
- **Stale temp files.** A killed repair leaves `.<name>.datfix-partial`. A running repair writes
  continuously, so "not modified for 60 s" separates a dead file from a live one.
  `remove_partials` refuses any name that is not a temp-file name.

### Gotchas met

- **`window.set_theme()` does not reach the page on macOS.** It changes the title bar, but
  WKWebView's `prefers-color-scheme` keeps following the system. Force the page theme with a
  `data-theme` attribute on `<html>` and write the dark tokens twice:
  `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {…} }` and
  `:root[data-theme="dark"] {…}`.
- **Testing system light mode.** `osascript -e 'tell application "System Events" to tell
  appearance preferences to set dark mode to false'` switches the whole Mac; set it back
  afterwards.
- **A drive root is not a camera card.** Treating every `/Volumes/<disk>` as a card made a
  `.DAT` on a 14 TB work disk scan the whole disk. Only folders with `DCIM`, `XFVC`,
  `PRIVATE` or `CONTENTS` count as a card.
- **The video edit list must come from the rebuilt tables.** Copying the reference's `elst`
  media time onto an All-Intra repair (no reordering) shifted playback by one frame, and
  players dropped a frame. Use `shift_frames × sample_delta` for video always.
- **`count_frames` on 4K ALL-I files decodes every frame.** Checking a few GB takes minutes;
  run it in the background.
- **The `.dmg` step fails in a session without Finder automation rights.** `bundle_dmg.sh`
  runs an AppleScript that lays out the Finder window. Without permission it fails and leaves
  `bundle/macos/rw.*.dmg`. `CI=true cargo tauri build --target universal-apple-darwin` skips
  that script; the `.dmg` is plain but valid (`hdiutil verify`).

## Keep the local toolchain current (2026-11-26)

CI installs the latest stable Rust. This Mac had 1.94 while CI had 1.100, so a new clippy lint
(`needless_range_loop`) failed CI but passed locally. Run `rustup update stable` before a work session,
or the local `cargo clippy` will not match CI.

## Hardening: fuzzing, failing cards, drives

Notes from making the engine survive hostile input and hostile environments.

### Fuzzing with cargo-fuzz

- `cargo-fuzz` drives libFuzzer. It needs a nightly toolchain:
  `rustup toolchain install nightly --profile minimal` and `cargo install cargo-fuzz --locked`
  (about 2 minutes together). Stable builds never see it.
- The fuzz crate lives in `crates/datfix-core/fuzz/` with its own `[workspace]`, so the main
  workspace ignores it. Its `.gitignore` keeps `corpus/`, `artifacts/`, `logs/` and `target/`
  out of git. Seeds cut from real media stay on the media disk, never in git.
- Run one target: `cargo +nightly fuzz run repi_walk fuzz/corpus/repi_walk <seed dir> --
  -max_total_time=900 -max_len=4194304 -rss_limit_mb=3072 -malloc_limit_mb=2048 -timeout=20`
  from `crates/datfix-core`. `-malloc_limit_mb` turns a huge allocation into a reported crash
  instead of a slow swap; `-timeout` turns a hang into one.
- Reproduce a crash: `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-<hash>`.
- Fuzz builds keep debug assertions, so integer overflow panics. Release builds wrap
  silently, which hides the bug and produces garbage. Fuzz or test in debug to catch them.
- Every input is "untrusted": sizes read from the file must be checked against the bytes
  left (`size > len - pos`, never `pos + size > len`, which overflows for a 64-bit size), and
  allocations must grow with the data found, not with a count the file claims
  (`Vec::with_capacity(count.min(1 << 20))`).
- Exp-Golomb `ue(v)` can code values up to 2^32 - 2. Any later `+ 1`, `* 16` or `as i32`
  overflows. Rejecting codes of 2^31 and up in the bit reader, plus spec range checks on the
  few fields that matter (picture size, POC deltas), removed a whole class of panics.
- Recursion on nested boxes needs a depth limit; a 100,000-deep `trak` chain is 800 KB.
- A fuzz target over the whole pipeline needs the pipeline to run on bytes in memory. Split
  "plan" (everything up to the bytes to write) from "write" and make the plan generic over
  `ReadAt`. The same split made the failing-card second pass easy.
- To let the fuzzer mutate two inputs at once (a `.DAT` and a reference clip), give the
  input a small header: `REF1` + u32 length + clip + `.DAT`.
- Keep a stable `cargo test` stand-in (`tests/fuzz_smoke.rs`): a fixed-seed xorshift
  mutator over synthetic files hits the same entry points in a few seconds, so CI catches
  regressions without nightly.

### Failing cards

- A failing SD/CFexpress card returns `EIO` (raw OS error 5) for unreadable sectors. A
  removed card returns other errors (`ENXIO`, `ENOENT`). Only skip the first kind.
- Mark unreadable regions in 64 KiB blocks and double the skip on consecutive failures (up
  to 16 MiB), as ddrescue's first pass does. Each failed read can take seconds on a real
  card; one read per sector would take hours over a dead region.
- Never re-read a region known to be bad; return zeros for it.
- Stop when a quarter of the file is unreadable: the card is gone, and a "repair" made of
  zeros helps nobody.
- A bad region found while copying the payload means the tables already point at zeros.
  Plan again around it and write a second file, reading the payload back from the first
  output instead of from the card. Name that file like any other temp file
  (`.<name>.pass1.datfix-partial`), so the stale-file cleanup finds it after a crash.
- Simulate a failing card with an in-memory `ReadAt` that returns
  `io::Error::from_raw_os_error(5)` (23 on Windows) inside chosen ranges.

### Output drives

- FAT32 cannot hold a file of 4 GiB or more; the write fails at exactly 4 GiB with `EFBIG`
  (`ErrorKind::FileTooLarge`). Detect the file system up front: `statfs` `f_fstypename` is
  `msdos` on macOS; Linux reports `f_type` 0x4d44; Windows `GetVolumeInformationW` returns
  `FAT32`. The `libc` and `windows-sys` crates give the calls; gate them with
  `#[cfg(target_os = ...)]` and `[target.'cfg(windows)'.dependencies]`.
- `statfs` field widths differ between platforms (`u32` on some), so a cast that is needless
  on macOS is needed elsewhere. Allow `clippy::unnecessary_cast` on that function only.
- Probe that the output folder accepts files by creating and deleting a small file before a
  long job. It catches locked cards, read-only shares and missing folders in milliseconds.
- File names are limited to 255 bytes. A hidden temp name built as `.<name>.datfix-partial`
  can pass the limit when the user's name is long; shorten on a `char` boundary and add a
  short hash so two long names stay distinct.
- Rust's standard library adds the `\\?\` prefix for long absolute paths on Windows by
  itself; no special handling is needed for paths over 260 characters.
- Test drives on macOS without root: `hdiutil create -size 64m -fs "MS-DOS FAT32"` and
  `hdiutil attach -nobrowse` (FAT32), a 150 MB HFS+ image with three concurrent repairs
  (disk full mid-write), `hdiutil attach -readonly` (read-only volume). Detach afterwards.

### Network shares

- Over SMB, thousands of tiny header reads dominate the index walk, not bandwidth.
  macOS's read-ahead makes each tiny read pull a large block; `fcntl(fd, F_RDAHEAD, 0)` for
  the walk (and back to 1 for the copy) made cold walks 1.5-2x faster.
- Measuring cold-cache times on a share is hard: the first read of a file warms the
  client cache, and `purge` needs root. Compare different files of the same camera and size,
  and record s/GB.

### Toolchain drift

- Another session ran `rustup update stable` (1.94 to 1.100) in the middle of this work.
  The newer clippy flagged older code (`needless_range_loop`), and builds failed for a
  minute with "the 'rustc' binary is not applicable" while the update ran. CI should pin
  the toolchain (`rust-toolchain.toml`) if exact clippy results matter.
- `cargo check --target x86_64-pc-windows-msvc` works from a Mac for pure-Rust crates after
  `rustup target add x86_64-pc-windows-msvc`. The Tauri app's build script also needs
  `llvm-rc` (a Windows resource compiler), so it does not check from a Mac without it.

## Engine round 3 (metadata, dates, rotation)

### Privacy of copied boxes

- A Canon camera MP4 carries a `uuid` box (`85c0b687-820f-11e0-8111-f4ce462b6a48`) in its
  `moov`. Inside: `CNCV` (compressor version), `CNDM`, `CNTH` (a JPEG thumbnail whose EXIF
  holds the body model, firmware, battery, lens name and serial, body serial and the capture
  dates with time zone) and `CNOP` (another copy of the body serial). Copying the reference's
  `moov` "verbatim except the tables" therefore stamps another clip's identity onto every
  repair. Treat any box you copy from a user's file as personal data until shown otherwise.
- QuickTime/AVFoundation, ffmpeg and the built-in profiles (which never had the box) all play
  files without it. Old exiftool (11.38) shows only `CompressorVersion` and `ThumbnailImage`
  from it; `strings` on the first megabyte is a better leak check.
- Removing children of `moov` shifts the index of every later child. Code that stores
  "track = n-th child of moov" (`RefTrack::moov_index`) must skip boxes while it builds the
  output, not edit the reference tree beforehand.
- When rewriting a `udta`, keep only what you can vouch for (`manu`, `modl`), and write the
  model from the `.DAT`'s own body code, not from the reference.

### Time

- FAT32 and exFAT timestamps share the DOS layout: date = `(year-1980)<<9 | month<<5 | day`,
  time = `hour<<11 | minute<<5 | seconds/2`, plus a 10 ms byte (0–199). exFAT adds a
  time-zone byte: bit 7 = valid, low 7 bits a signed count of 15-minute steps. FAT32 has no
  zone, so its times are camera local time and stay that way.
- A local time without a zone can still be tied to UTC when a second clock agrees: round the
  difference to a real zone (multiples of 30 min plus +5:45, +8:45, +12:45) and accept only a
  small remainder. Rounding to 15 minutes accepts everything (any value is within 7.5 min of
  a quarter hour), so it is not a check at all.
- Canon timecode in `tmcd` is usually the camera clock (frames / real fps = time of day), but
  some clips use a preset timecode (LAD18029: 6 h 51 min off). The per-frame records and
  `tdmt` hold no date.
- Do not use a carved file's own modification time: it is the time it was extracted.

### Display rotation

- Orientation lives only in the `tkhd` matrix `{a b u; c d v; tx ty w}` (16.16 fixed point,
  `w` 2.30). For a clockwise turn of a W×H picture: 90° = `0,1,-1,0, tx=H`; 180° =
  `-1,0,0,-1, tx=W, ty=H`; 270° = `0,-1,1,0, ty=W`. Keep `tkhd` width and height unrotated.
- ffprobe reports the display matrix as `rotation=-90` for a 90° clockwise turn (ffmpeg
  counts counter-clockwise). AVFoundation's `preferredTransform` shows `[0 1 -1 0]`.

### Small Rust notes

- `#[serde(into = "u32", try_from = "u32")]` serializes an enum such as `Rotation` as a
  plain number and rejects bad values on input, with no hand-written serde code.
- A `\` at the end of a line inside a Python string written with a heredoc is a Python line
  continuation, not a Rust one: generated Rust strings lost their newline and kept the
  indentation. Check generated string literals for runs of spaces.
- Adding a field to a public struct breaks callers that build it with a literal. Give new
  option structs `Default` and tell callers to use `..Default::default()`.
