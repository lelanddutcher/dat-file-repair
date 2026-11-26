# Corpus regression and hardening results

Date: 2026-11-26. Branch `feat/hardening`. Binary tested: `98ce494` (release build).

## 1. What this document is

DATfix repairs Canon `.DAT` files. A `.DAT` is a recording that the camera did not finish:
the media is there, but the index that players need (`moov`) is missing. DATfix rebuilds
the index and writes a new `.MP4`.

The **corpus** is the set of 36 real `.DAT` files from our NAS, copied read-only to
`/Volumes/15T/dat-file-repair/corpus`. It covers three camera families (see
`docs/format/canon-dat-format.md`, section 4):

| Family | Camera | Layout | Files |
|---|---|---|---|
| A | EOS R3 | video only, padded samples | 10 |
| B | EOS R5 (10), EOS R3 (1) | timecode, 16-bit stereo PCM, video | 11 |
| C | EOS R5 Mark II | timecode, metadata, 4 x 24-bit PCM, video | 15 |

This regression repairs every file twice and checks every output. Run it after any change
to the engine. It runs by hand (about 30 minutes on the Mac Studio for 273 GB of input).

## 2. How a repair is checked

Two modes per `.DAT`:

- **auto** — `datfix repair <DAT> -o <out>`: try clips in the `.DAT`'s folder first, then the
  built-in settings.
- **builtin** — `datfix repair --builtin <DAT> -o <out>`: built-in settings only.

For each output (rules from `AGENTS.md`, "Verifying a repair"):

1. **Frames expected** = complete video samples in the `repi` index (`datfix inspect`).
2. **Packets** = `ffprobe -count_packets` video packet count. Must equal 1.
3. **Decoded** = frames counted by a full decode. Must equal 1. (This replaces
   `ffprobe -count_frames`, which decodes everything a second time.)
4. **Full decode** = `ffmpeg -v error -i out -map 0:v:0 -map 0:a? -f null -`, every frame of
   every file, the 38 GB ones included. Must print nothing.
5. **Durations** = every track's `duration_ts × time_base` within one video frame of the
   video track.
6. **Frames looked at** = a middle and a last frame JPEG for 11 files across all families
   and both modes (44 images). All show clean pictures, no blocks or smears.

Harness (not in git, next to its outputs): `/Volumes/15T/dat-file-repair/out/regress/regress.py`
(run) and `report.py` (tables). Command:

```sh
python3 -I regress.py <datfix binary> /Volumes/15T/dat-file-repair/corpus <out dir> --jobs 6
python3 -I report.py <out dir>/results.jsonl [<baseline results.jsonl>]
```

Outputs above 2 GB are deleted after their checks; smaller ones stay in
`/Volumes/15T/dat-file-repair/out/regress/run2/{auto,builtin}`.

## 3. Summary

| Check | Result |
|---|---|
| Repairs that passed every check, auto mode | **36 / 36** |
| Repairs that passed every check, builtin mode | **36 / 36** |
| Full decodes with zero ffmpeg errors | 72 / 72 (111 min of decoding) |
| Frame count = index count (packets and decoded) | 72 / 72 |
| Track durations equal within one frame | 72 / 72 |
| `moov` byte-identical to the baseline run before hardening (`ad44eee`) | 72 / 72 |
| Repair time, all 72 | 635 s for 273 GB, 430 MB/s overall (local 15T disk) |
| Failures | none |

Settings used in auto mode: a camera clip from the same folder for 20 files, the built-in
settings for 16 (folders without a matching original clip: the 10 family B R5 files,
`LAD33743`, `LAD33835`, `A_0001`, `A_0002`, `A_0015`, `A_0028`).

**Dropped-frame bug re-check.** Commit `577ce38` fixed All-Intra recordings repaired with
Long-GOP settings: the copied edit list hid the first frame. Affected here: the 10 family B
R5 files (All-Intra, built-in settings `R5-…-lgop-mp4a`) in both modes, 20 outputs. All 20
decode exactly the index frame count, and the first video frame has PTS 0.000 (spot-checked
`LAD11225`, `LAD12023`, `LAD33743`, `LAD37395`).

**Baseline.** The same harness ran first on `ad44eee` (main before this branch): also 72/72.
The hardening changes did not change one byte of any output header.

## 4. Per-file results (run 2, `98ce494`)

Family A = R3 video only, B = CAEP + PCM, C = R5 Mark II XF-HEVC S. "Tracks" counts the
output's streams. Repair time includes the index walk, the frame analysis and the write.

| # | .DAT | Family | Camera | Size (GB) | Mode | Settings from | Frames expected | Packets | Decoded | Tracks | Durations | Full decode | Repair (s) | MB/s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | LAD39665.DAT | A | EOS R3 | 0.13 | auto | clip LAD39658.MP4 | 67 | 67 | 67 | 1 | equal | clean | 0.41 | 328.2 |
| 2 | LAD39665.DAT | A | EOS R3 | 0.13 | builtin | built-in R3 | 67 | 67 | 67 | 1 | equal | clean | 0.17 | 799.9 |
| 3 | LAD37406.DAT | A | EOS R3 | 0.27 | auto | clip LAD37394.MP4 | 136 | 136 | 136 | 1 | equal | clean | 0.57 | 467.1 |
| 4 | LAD37406.DAT | A | EOS R3 | 0.27 | builtin | built-in R3 | 136 | 136 | 136 | 1 | equal | clean | 0.54 | 495.0 |
| 5 | LAD37395.DAT | A | EOS R3 | 0.37 | auto | clip LAD37394.MP4 | 188 | 188 | 188 | 1 | equal | clean | 0.79 | 468.0 |
| 6 | LAD37395.DAT | A | EOS R3 | 0.37 | builtin | built-in R3 | 188 | 188 | 188 | 1 | equal | clean | 0.83 | 446.5 |
| 7 | LAD37708.DAT | A | EOS R3 | 0.60 | auto | clip LAD37709.MP4 | 308 | 308 | 308 | 1 | equal | clean | 0.99 | 608.8 |
| 8 | LAD37708.DAT | A | EOS R3 | 0.60 | builtin | built-in R3 | 308 | 308 | 308 | 1 | equal | clean | 1.36 | 445.2 |
| 9 | LAD39659.DAT | A | EOS R3 | 1.74 | auto | clip LAD39658.MP4 | 901 | 901 | 901 | 1 | equal | clean | 3.42 | 510.9 |
| 10 | LAD39659.DAT | A | EOS R3 | 1.74 | builtin | built-in R3 | 901 | 901 | 901 | 1 | equal | clean | 4.08 | 427.8 |
| 11 | LAD39177.DAT | A | EOS R3 | 1.78 | auto | clip LAD39648.MP4 | 910 | 910 | 910 | 1 | equal | clean | 3.91 | 455.2 |
| 12 | LAD39177.DAT | A | EOS R3 | 1.78 | builtin | built-in R3 | 910 | 910 | 910 | 1 | equal | clean | 4.2 | 423.3 |
| 13 | LAD39649.DAT | A | EOS R3 | 2.48 | auto | clip LAD39648.MP4 | 1269 | 1269 | 1269 | 1 | equal | clean | 5.31 | 468.0 |
| 14 | LAD39649.DAT | A | EOS R3 | 2.48 | builtin | built-in R3 | 1269 | 1269 | 1269 | 1 | equal | clean | 5.86 | 423.8 |
| 15 | LAD37315.DAT | A | EOS R3 | 6.44 | auto | clip LAD37317.MP4 | 3303 | 3303 | 3303 | 1 | equal | clean | 15.22 | 423.3 |
| 16 | LAD37315.DAT | A | EOS R3 | 6.44 | builtin | built-in R3 | 3303 | 3303 | 3303 | 1 | equal | clean | 15.22 | 423.4 |
| 17 | LAD39662.DAT | A | EOS R3 | 13.02 | auto | clip LAD39658.MP4 | 6674 | 6674 | 6674 | 1 | equal | clean | 32.79 | 397.0 |
| 18 | LAD39662.DAT | A | EOS R3 | 13.02 | builtin | built-in R3 | 6674 | 6674 | 6674 | 1 | equal | clean | 30.57 | 425.9 |
| 19 | LAD33835.DAT | A | EOS R3 | 31.88 | auto | built-in R3 | 16383 | 16383 | 16383 | 1 | equal | clean | 77.52 | 411.2 |
| 20 | LAD33835.DAT | A | EOS R3 | 31.88 | builtin | built-in R3 | 16383 | 16383 | 16383 | 1 | equal | clean | 79.0 | 403.5 |
| 21 | LAD11225.DAT | B | EOS R5 | 0.07 | auto | built-in R5 | 30 | 30 | 30 | 3 | equal | clean | 0.11 | 614.2 |
| 22 | LAD11225.DAT | B | EOS R5 | 0.07 | builtin | built-in R5 | 30 | 30 | 30 | 3 | equal | clean | 0.06 | 1107.8 |
| 23 | LAD18029.DAT | B | EOS R5 | 0.13 | auto | built-in R5 | 63 | 63 | 63 | 3 | equal | clean | 0.24 | 568.9 |
| 24 | LAD18029.DAT | B | EOS R5 | 0.13 | builtin | built-in R5 | 63 | 63 | 63 | 3 | equal | clean | 0.15 | 922.6 |
| 25 | LAD18036.DAT | B | EOS R5 | 0.20 | auto | built-in R5 | 94 | 94 | 94 | 3 | equal | clean | 0.44 | 458.5 |
| 26 | LAD18036.DAT | B | EOS R5 | 0.20 | builtin | built-in R5 | 94 | 94 | 94 | 3 | equal | clean | 0.42 | 479.1 |
| 27 | LAD11983.DAT | B | EOS R5 | 0.34 | auto | built-in R5 | 159 | 159 | 159 | 3 | equal | clean | 0.52 | 647.8 |
| 28 | LAD11983.DAT | B | EOS R5 | 0.34 | builtin | built-in R5 | 159 | 159 | 159 | 3 | equal | clean | 0.45 | 742.5 |
| 29 | LAD12203.DAT | B | EOS R5 | 0.50 | auto | built-in R5 | 240 | 240 | 240 | 3 | equal | clean | 0.7 | 719.4 |
| 30 | LAD12203.DAT | B | EOS R5 | 0.50 | builtin | built-in R5 | 240 | 240 | 240 | 3 | equal | clean | 1.02 | 493.0 |
| 31 | LAD11582.DAT | B | EOS R5 | 0.64 | auto | built-in R5 | 303 | 303 | 303 | 3 | equal | clean | 0.93 | 686.4 |
| 32 | LAD11582.DAT | B | EOS R5 | 0.64 | builtin | built-in R5 | 303 | 303 | 303 | 3 | equal | clean | 1.38 | 460.3 |
| 33 | LAD12023.DAT | B | EOS R5 | 0.70 | auto | built-in R5 | 337 | 337 | 337 | 3 | equal | clean | 1.02 | 692.1 |
| 34 | LAD12023.DAT | B | EOS R5 | 0.70 | builtin | built-in R5 | 337 | 337 | 337 | 3 | equal | clean | 1.5 | 470.5 |
| 35 | LAD33743.DAT | B | EOS R3 | 0.77 | auto | built-in R3 | 368 | 368 | 368 | 3 | equal | clean | 1.28 | 603.1 |
| 36 | LAD33743.DAT | B | EOS R3 | 0.77 | builtin | built-in R3 | 368 | 368 | 368 | 3 | equal | clean | 0.89 | 867.6 |
| 37 | LAD11830.DAT | B | EOS R5 | 1.21 | auto | built-in R5 | 577 | 577 | 577 | 3 | equal | clean | 1.4 | 862.8 |
| 38 | LAD11830.DAT | B | EOS R5 | 1.21 | builtin | built-in R5 | 577 | 577 | 577 | 3 | equal | clean | 1.54 | 783.3 |
| 39 | LAD10430.DAT | B | EOS R5 | 1.21 | auto | built-in R5 | 578 | 578 | 578 | 3 | equal | clean | 2.24 | 539.2 |
| 40 | LAD10430.DAT | B | EOS R5 | 1.21 | builtin | built-in R5 | 578 | 578 | 578 | 3 | equal | clean | 2.8 | 431.7 |
| 41 | LAD11981.DAT | B | EOS R5 | 1.21 | auto | built-in R5 | 578 | 578 | 578 | 3 | equal | clean | 2.4 | 502.7 |
| 42 | LAD11981.DAT | B | EOS R5 | 1.21 | builtin | built-in R5 | 578 | 578 | 578 | 3 | equal | clean | 1.69 | 715.7 |
| 43 | A_0002D054H241221_134042C0_LAD04.DAT | C | EOS R5 Mark II | 0.07 | auto | built-in R5m2 | 135 | 135 | 135 | 7 | equal | clean | 0.14 | 491.5 |
| 44 | A_0002D054H241221_134042C0_LAD04.DAT | C | EOS R5 Mark II | 0.07 | builtin | built-in R5m2 | 135 | 135 | 135 | 7 | equal | clean | 0.05 | 1224.4 |
| 45 | B_0004C544H241109_133743FE_LAD04.DAT | C | EOS R5 Mark II | 0.20 | auto | clip B_0004C511H241109_123428VG_LAD04.MP4 | 420 | 420 | 420 | 7 | equal | clean | 0.44 | 454.7 |
| 46 | B_0004C544H241109_133743FE_LAD04.DAT | C | EOS R5 Mark II | 0.20 | builtin | built-in R5m2 | 420 | 420 | 420 | 7 | equal | clean | 0.22 | 931.5 |
| 47 | A_0037C773H251016_1231241N_LAD04.DAT | C | EOS R5 Mark II | 0.50 | auto | clip A_0037C785H251016_123954EJ_LAD04.MP4 | 1057 | 1057 | 1057 | 7 | equal | clean | 0.76 | 661.7 |
| 48 | A_0037C773H251016_1231241N_LAD04.DAT | C | EOS R5 Mark II | 0.50 | builtin | built-in R5m2 | 1057 | 1057 | 1057 | 7 | equal | clean | 0.99 | 507.4 |
| 49 | A_0001C809H241214_1422235Q_LAD04.DAT | C | EOS R5 Mark II | 0.54 | auto | built-in R5m2 | 1127 | 1127 | 1127 | 7 | equal | clean | 0.84 | 639.9 |
| 50 | A_0001C809H241214_1422235Q_LAD04.DAT | C | EOS R5 Mark II | 0.54 | builtin | built-in R5m2 | 1127 | 1127 | 1127 | 7 | equal | clean | 1.2 | 446.7 |
| 51 | A_0037C785H251016_123919P5_LAD04.DAT | C | EOS R5 Mark II | 0.74 | auto | clip A_0037C785H251016_123954EJ_LAD04.MP4 | 1555 | 1555 | 1555 | 7 | equal | clean | 1.18 | 623.1 |
| 52 | A_0037C785H251016_123919P5_LAD04.DAT | C | EOS R5 Mark II | 0.74 | builtin | built-in R5m2 | 1555 | 1555 | 1555 | 7 | equal | clean | 1.59 | 465.1 |
| 53 | A_0025C582H250531_110712X4_LAD04.DAT | C | EOS R5 Mark II | 0.87 | auto | clip A_0025C583H250531_110849FE_LAD04.MP4 | 1833 | 1833 | 1833 | 7 | equal | clean | 1.75 | 497.2 |
| 54 | A_0025C582H250531_110712X4_LAD04.DAT | C | EOS R5 Mark II | 0.87 | builtin | built-in R5m2 | 1833 | 1833 | 1833 | 7 | equal | clean | 1.12 | 775.8 |
| 55 | A_0053C607H260110_115130LJ_LAD04.DAT | C | EOS R5 Mark II | 0.97 | auto | clip A_0053C606H260110_114719SP_LAD04.MP4 | 2045 | 2045 | 2045 | 7 | equal | clean | 1.98 | 490.9 |
| 56 | A_0053C607H260110_115130LJ_LAD04.DAT | C | EOS R5 Mark II | 0.97 | builtin | built-in R5m2 | 2045 | 2045 | 2045 | 7 | equal | clean | 2.29 | 424.0 |
| 57 | A_0026C677H250623_101219EK_LAD04.DAT | C | EOS R5 Mark II | 1.51 | auto | clip A_0026C677H250623_101318EJ_LAD04.MP4 | 3177 | 3177 | 3177 | 7 | equal | clean | 3.57 | 422.5 |
| 58 | A_0026C677H250623_101219EK_LAD04.DAT | C | EOS R5 Mark II | 1.51 | builtin | built-in R5m2 | 3177 | 3177 | 3177 | 7 | equal | clean | 0.72 | 2086.5 |
| 59 | A_0024C180H250508_174627EJ_LAD04.DAT | C | EOS R5 Mark II | 1.58 | auto | clip A_0024C178H250508_1710055E_LAD04.MP4 | 3321 | 3321 | 3321 | 7 | equal | clean | 3.76 | 419.1 |
| 60 | A_0024C180H250508_174627EJ_LAD04.DAT | C | EOS R5 Mark II | 1.58 | builtin | built-in R5m2 | 3321 | 3321 | 3321 | 7 | equal | clean | 2.64 | 596.6 |
| 61 | B_0004C469H241109_111822E8_LAD04.DAT | C | EOS R5 Mark II | 1.64 | auto | clip B_0004C511H241109_123428VG_LAD04.MP4 | 3461 | 3461 | 3461 | 7 | equal | clean | 3.8 | 432.4 |
| 62 | B_0004C469H241109_111822E8_LAD04.DAT | C | EOS R5 Mark II | 1.64 | builtin | built-in R5m2 | 3461 | 3461 | 3461 | 7 | equal | clean | 3.85 | 427.2 |
| 63 | B_0004C512H241109_123729EJ_LAD04.DAT | C | EOS R5 Mark II | 2.85 | auto | clip B_0004C511H241109_123428VG_LAD04.MP4 | 6004 | 6004 | 6004 | 7 | equal | clean | 7.18 | 397.1 |
| 64 | B_0004C512H241109_123729EJ_LAD04.DAT | C | EOS R5 Mark II | 2.85 | builtin | built-in R5m2 | 6004 | 6004 | 6004 | 7 | equal | clean | 5.88 | 485.3 |
| 65 | B_0004C512H241109_123515LE_LAD04.DAT | C | EOS R5 Mark II | 5.84 | auto | clip B_0004C511H241109_123428VG_LAD04.MP4 | 12303 | 12303 | 12303 | 7 | equal | clean | 6.37 | 916.9 |
| 66 | B_0004C512H241109_123515LE_LAD04.DAT | C | EOS R5 Mark II | 5.84 | builtin | built-in R5m2 | 12303 | 12303 | 12303 | 7 | equal | clean | 12.65 | 461.4 |
| 67 | A_0028C829H250627_1255235E_LAD04.DAT | C | EOS R5 Mark II | 6.07 | auto | built-in R5m2 | 12820 | 12820 | 12820 | 7 | equal | clean | 18.79 | 323.3 |
| 68 | A_0028C829H250627_1255235E_LAD04.DAT | C | EOS R5 Mark II | 6.07 | builtin | built-in R5m2 | 12820 | 12820 | 12820 | 7 | equal | clean | 14.29 | 425.1 |
| 69 | A_0015C453H250312_151308LJ_LAD04.DAT | C | EOS R5 Mark II | 9.70 | auto | built-in R5m2 | 20464 | 20464 | 20464 | 7 | equal | clean | 26.14 | 370.9 |
| 70 | A_0015C453H250312_151308LJ_LAD04.DAT | C | EOS R5 Mark II | 9.70 | builtin | built-in R5m2 | 20464 | 20464 | 20464 | 7 | equal | clean | 16.05 | 604.0 |
| 71 | A_0037C750H251016_104856J4_LAD04.DAT | C | EOS R5 Mark II | 37.78 | auto | clip A_0037C785H251016_123954EJ_LAD04.MP4 | 79727 | 79727 | 79727 | 7 | equal | clean | 100.31 | 376.6 |
| 72 | A_0037C750H251016_104856J4_LAD04.DAT | C | EOS R5 Mark II | 37.78 | builtin | built-in R5m2 | 79727 | 79727 | 79727 | 7 | equal | clean | 88.69 | 426.0 |

## 5. Findings

### 5.1 Vertical recordings play sideways with the built-in settings

- **What:** The orientation of a clip (camera held upright for a vertical shot) is stored
  only in the finished file's track header (`tkhd` matrix). A `.DAT` has no track header.
- **Seen in:** the frame checks of `LAD33835` (both modes, no clip in its folder) and
  `LAD37395` (builtin mode). Any vertical recording repaired with the built-in settings is
  affected. In auto mode with a vertical neighbour clip (`LAD37394.MP4`, rotation 90), the output
  plays upright, because the clip's matrix is copied.
- **Tried:** compared the first per-frame record of 6 upright and 13 rotated R3 clips
  (`research-scripts/records_vs_rotation.py`). No byte is constant within each group and
  different between them. The `.DAT` does not record orientation.
- **User-visible result:** the picture is correct but turned 90 degrees. Every editor can
  rotate it.
- **Smallest fix (not done, needs a product decision):** a "Rotate" choice in the app and a
  `--rotate 90|180|270` CLI flag that writes the matrix.

### 5.2 Network shares (SMB)

Measured on `/Volumes/cliplogger-testing-disk/dat-file-repair/corpus` (read-only), output to
the local 15T disk:

| Step | Result |
|---|---|
| Sequential read, `dd bs=8m`, 2 GiB | 348 MB/s |
| `A_0015C453…` (9.0 GB) cold index walk, before | 38 s |
| `A_0015C453…` full repair (index again + analysis + 9.7 GB write) | 34 s, 285 MB/s |
| Cold index walk, before this branch | 6.4-9.7 s/GB (3 files) |
| Cold index walk, after this branch | 4.4-4.9 s/GB (3 files of the same cameras and sizes) |

The payload copy already used 8 MiB reads. The index walk was slow because of thousands
of tiny reads, each pulling a macOS read-ahead block over the network. Fixes: read-ahead is
off for the walk and analysis (`F_RDAHEAD`), and the walker confirms a known sample
alignment on 5 samples instead of checking up to 128.

## 6. Hardening done on this branch

### 6.1 Fuzzing

Four `cargo-fuzz` targets in `crates/datfix-core/fuzz` (nightly):

| Target | Input | Code covered |
|---|---|---|
| `repi_walk` | `.DAT` bytes | walker, stream shapes, layout, timeline, inspect summary |
| `mp4_reference` | `.MP4` bytes | top-level scan, `moov` tree, tracks, sample tables, codec config, renamed-MP4 check |
| `codec_headers` | header bytes | `hvcC`/`avcC`, HEVC/H.264 SPS/PPS, slice headers (against the built-in profiles' real parameter sets), frame verification and analysis |
| `repair_plan` | `.DAT` bytes, optionally with a reference clip | the whole repair up to writing |

Seeds: synthetic files from the test builders plus heads of real `.DAT` files and real
Canon clip headers (kept on the media disk, not in git).

Two rounds of 15 minutes, all four targets in parallel (2 hours of target time, about
29 million executions). The first short runs found these, all fixed:

| Crash | Cause | Fix |
|---|---|---|
| add overflow, `layout.rs` codec guess | NAL length near `u32::MAX` + 4 | widen to `u64` |
| add overflow, `atom.rs` top-level scan | position + 8 after a huge box | saturating subtraction |
| multiply overflow, `avc.rs` SPS | `(ue + 1) * 16` with a huge `ue` | `ue` values of 2^31 and up rejected; size range check |
| add overflow, `atom.rs` box parser (found after 10 min) | 64-bit box size near `u64::MAX` | compare with bytes left |
| possible huge allocations | `ctts`/`stsz` counts from the file | cap at 2^26 samples, grow lazily |
| possible stack overflow | deeply nested containers | depth limit 32 |
| overflow risks in HEVC RPS/POC math | unbounded deltas | spec range checks |

Round 2 ran on the final code: **zero crashes, zero timeouts, zero out-of-memory**.
`tests/fuzz_smoke.rs` repeats the four entry points on mutated synthetic files in stable
`cargo test` (debug build, so overflow panics).

### 6.2 Malformed and unusual input

Each case has a test in `crates/datfix-core/tests/edge_cases.rs` and a plain message:

| Case | Result |
|---|---|
| Reference is the `.DAT` itself | `not_a_clip`: "is the broken .DAT itself" |
| Reference is another `.DAT` | `not_a_clip`: "an unfinished recording" |
| Reference is a JPEG, PNG, TIFF/RAW, CR3 or `.CRM` (brand `crx `), HEIF, Matroska, zeros | `not_a_clip` naming the type |
| Reference cut short | `bad_reference`: "cut short or still copying" |
| Reference is random bytes (7 sizes) | `bad_reference` or `not_a_clip`, never a panic |
| Reference is a QuickTime `.MOV` with matching tracks | repairs normally |
| `.DAT` is a renamed playable MP4 | `already_playable`: copy it with an `.MP4` name |
| `.DAT` starts with 3 MiB of zeros (carved from a card image) | repairs; payload starts at the first group |
| 0-byte or all-zero `.DAT` | `empty_dat` with PhotoRec advice |
| Header only, header cut in the size table, cut inside the first frame | `no_frames` |

### 6.3 Environment

| Case | Test | Result |
|---|---|---|
| Output on FAT32, file over 4 GiB | unit test + real FAT32 disk image with the 9.7 GB repair | refused in 82 ms: "can't store files larger than 4 GB" |
| Not enough free space | unit test; core check before the walk and with the exact size | `disk_full` before writing |
| Disk full mid-write | real 150 MB HFS+ image, three repairs at once | "The disk is full"; no temporary file left |
| Read error mid-file (failing card) | `tests/damage.rs`, simulated EIO inside video and inside audio | skipped as damage by default; sync kept sample by sample; report lists unreadable bytes and seconds lost; `--stop-on-read-error` stops with `read_error` |
| Card removed or mostly dead | `tests/damage.rs` | `too_much_unreadable` after a quarter of the file |
| Input on read-only media | test (0444 file, 0555 folder) + read-only disk image | input only read; repair to another folder works |
| Output folder not writable / missing | test + read-only disk image | `output_not_writable`: "Choose another folder" |
| Non-ASCII, spaces, long names (`Hochzeit Müller/Trauung – Kirche/クリップ 01.DAT`, 252-byte output name) | test | repairs; temporary name kept under 255 bytes |
| Windows long paths (`\\?\`) | none needed | Rust's standard library adds the prefix itself |
| Windows build | `cargo clippy --target x86_64-pc-windows-msvc -p datfix-core -p datfix --all-targets -D warnings` | clean. `datfix-app` not checked: its build script needs `llvm-rc`, which is not installed on the Mac |

## 7. Not done

- Rotation for vertical recordings (5.1): needs a product decision.
- `datfix-app` Windows check: needs `llvm-rc` (or a Windows machine).
