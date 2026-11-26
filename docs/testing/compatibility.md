# Reference clips and editing-app compatibility

Status: tested 2026-11-26 with the `datfix` release build from `main` (`fd4b7ff`).

## What this covers

`datfix repair` rebuilds an MP4 from a Canon `.DAT` file. It needs a **reference clip**: one
healthy MP4 that the same camera body recorded in the same mode. The reference supplies the codec
setup (`hvcC`/`avcC`), the track layout and the Canon metadata box.

This page answers two questions:

1. Which camera-original reference clips exist on the NAS, and which corpus `.DAT` files can now be
   repaired with them?
2. Do repaired files open in the editing apps people use (QuickTime/AVFoundation, DaVinci Resolve,
   Premiere Pro), and do they behave like camera originals?

A **camera original** has a Canon `ftyp` brand (`CAEP` or `XFVC`), a Canon `uuid` box and `repi`
groups at the start of `mdat`. Re-encodes (AV1, `hev1`, `isom` brand) are not references.

Media and working files are outside git, under `/Volumes/15T/dat-file-repair/`:

- `refs/inventory.tsv`: every Canon original found on the NAS (13,827 rows).
- `refs/<body>/<mode>/`: 1–2 small copies per mode, mode `0444`.
- `refs/scripts/`: the read-only scan, check and proof scripts used here.
- `out/compat/`: thumbnails, metadata dumps and decode logs for job 2.

## Results: editing apps

"Original" is a camera original in the same mode, tested the same way, so differences are
attributable to the repair. All originals passed every check.

| File | Mode | ffmpeg full decode | QuickTime / AVFoundation | DaVinci Resolve 21.2 | Premiere Pro |
|---|---|---|---|---|---|
| `r5m2_64MB_repaired.MP4` | R5 II, 4K DCI 119.88p, 4× 24-bit PCM | Pass, 0 errors | Pass | Pass | Not run |
| `B_0004C512_123515LE_repaired.MP4` (5.8 GB) | same | Pass, 0 errors | Pass | Pass | Not run |
| `LAD37315_repaired.MP4` (6.4 GB) | R3, 4K DCI 29.97p ALL-I, no audio | Pass, 0 errors | Pass | Pass | Not run |
| Family B R5 repair | R5, 4K DCI 59.94p ALL-I, LPCM | — | — | — | — |
| Original `A_0026C730…_LAD04.MP4` | R5 II 119.88p | Pass | Pass | Pass | Not run |
| Original `LAD37341.MP4` | R3 29.97p | Pass | Pass | Pass | Not run |

No family B R5 file could be made. `datfix` rejects every family B `.DAT` (issue 1).

### QuickTime / AVFoundation

A Swift script (`refs/scripts/avcheck.swift`) loads each file with `AVURLAsset`. It reads
`isPlayable`, the tracks and their durations. It saves frames at 50 % and 97 % with
`AVAssetImageGenerator`, using zero time tolerance.

- All files: `isPlayable = true`, `isReadable = true`.
- R5 II files: 6 tracks: `hvc1` 4096×2160 119.880 fps, four mono 24-bit 48 kHz `lpcm` tracks, `tmcd`.
  All track durations are equal (1.126 s and 102.628 s).
- R3 file: 1 track, `hvc1` 4096×2160 29.970 fps, 110.210 s.
- AVFoundation hides the Canon `cndm` metadata track. The camera original does the same.
- All saved frames are clean. They are in `out/compat/av_*.jpg`.

### DaVinci Resolve Studio 21.2

All five clips were imported into a scratch project, `datfix-compat-test`. None was offline.
Clip properties:

| Clip | Resolution | FPS | Frames | Codec | Audio | Start TC |
|---|---|---|---|---|---|---|
| `r5m2_64MB_repaired` | 4096x2160 | 119.88 | 135 | H.265 Main 4:2:2 10 L6.1 | 4 ch LPCM 24-bit | 13:40:42;015 |
| `B_0004C512_123515LE_repaired` | 4096x2160 | 119.88 | 12303 | H.265 Main 4:2:2 10 L6.1 | 4 ch LPCM 24-bit | 12:35:15;013 |
| Original `A_0026C730` | 4096x2160 | 119.88 | 124 | H.265 Main 4:2:2 10 L6.1 | 4 ch LPCM 24-bit | 10:51:58;013 |
| `LAD37315_repaired` | 4096x2160 | 29.97 | 3303 | H.265 Main 4:2:2 10 L5.2 | none | 00:00:00:00 |
| Original `LAD37341` | 4096x2160 | 29.97 | 335 | H.265 Main 4:2:2 10 L5.2 | none | 00:00:00:00 |

- The start timecode comes from the `.DAT` itself. It matches the file name (`123515` → 12:35:15).
- A timeline built from the repaired R5 II clip and one built from the original are identical:
  1 video track, 4 audio tracks, one item on each.
- Thumbnails across the whole clip (6 per clip, first to last frame) were clean.
- The scratch project was closed and deleted afterwards.

### Premiere Pro

Not tested. Premiere Pro 2027 is installed but was not running. The MCP bridge timed out. It needs
the app open and the bridge panel started by hand ("Start Bridge").

### Metadata diff (exiftool `-a -G1 -s`, mediainfo `-f`)

Compared: `B_0004C512_123515LE_repaired.MP4` against original `A_0026C730…_LAD04.MP4`, and
`LAD37315_repaired.MP4` against original `LAD37341.MP4`. Dumps are in `out/compat/meta/`.

Same in both: brands, model (`Canon EOS R5m2`, `Canon EOS R3`), codec, profile, level, chroma, bit
depth, color fields, audio format, track count and order, timecode track.

Different:

| Field | Repaired | Original | Cause |
|---|---|---|---|
| `CreateDate` / track and media dates | `.DAT` modification time | recording start | issue 2 |
| Canon `uuid` thumbnail, EXIF date, firmware, lens | copied from the reference clip | this clip | issue 3 |
| GPS (`LocationInformation`) | from the reference clip (R3) | this clip | issue 3 |
| `MovieDataOffset` | 108–198 KB | 2,097,168 | `datfix` writes a compact `moov`; Canon reserves 2 MiB. Harmless. |
| `sdtp` (sample dependency) | absent | present | dropped by design (CLI note). No effect seen in any app. |
| `free` box | absent | present | harmless |

ffmpeg prints `Missing key frame while searching for timestamp: 1001` when it seeks in both the
repaired files and the camera originals. It is the Canon edit list, not a repair defect.

## Reference inventory

The scan read the first 32 bytes of every `.MP4/.CRM/.MOV/.MXF` file on both shares, server side. For
Canon files it parsed `moov`. Copies in ClipLogger test folders are included in the counts.

| Body | Files | Modes found (video / fps / GOP / audio) |
|---|---|---|
| R5 Mark II (`XFVC`) | 3,621 | HEVC 4:2:2 10-bit 4K DCI at 119.88p (2,748), 59.94p (657), 24p (169), 29.97p (10); HEVC 4:2:0 4K 24p (7); HEVC 8K 24p (12); HEVC 2K 59.94p (1) and 239.76p (1); H.264 4:2:2 4K 59.94p (15); H.264 1080p 29.97 AAC (1). All Long GOP. All 4× mono 24-bit `ipcm` except the last. |
| R3 (`CAEP`) | 899 | HEVC 4:2:2 10-bit 4K DCI ALL-I: 29.97p no audio (397), 59.94p `twos` (360), 59.94p no audio (90), 24p `twos` (3). HEVC 4K 59.94p Long GOP `twos` (41). H.264 1080p 29.97 AAC (8). |
| R5 (`CAEP`) | 537 | HEVC 4:2:2 10-bit 4K DCI: 29.97p ALL-I no audio (158), 59.94p ALL-I AAC (119), **59.94p ALL-I `twos` (113)**, 59.94p Long GOP AAC (101), 59.94p Long GOP `twos` (10), 24p (7). HEVC UHD 59.94p ALL-I `twos` (2). HEVC 8K 24p ALL-I (10). H.264 4K and 1080p (16). |
| R6 Mark II | 189 | HEVC UHD 59.94p Long GOP `twos` (148); H.264 UHD 59.94p/23.976p AAC (38); others (3). |
| R6 | 54 | HEVC 1080p 59.94p AAC (42); H.264 1080p/UHD (9); others (3). |
| R5 C | 49 | HEVC 4K 59.94p Long GOP, 4× mono `twos` (40) or AAC (9). |
| C80 | 36 | HEVC 2K 59.94p, AAC (28) or 4× `ipcm` (8). |
| C70 | 3 | HEVC 1080p 59.94p AAC. |
| EOS R, M50 II, 850D, T7i | 10 | H.264 1080p, no `repi` (family D) or minor. |
| Cinema RAW Light `.CRM` | 353 | 6000×3164 59.94p (243, R3 size) and 8192×4320 24/29.97p (100, R5 size). No `modl` string. |
| Older Canon `.MOV` (`qt  CAEP`) | 8,076 | 5D Mark IV, 1D X Mark II, 6D, 70D. MJPEG or H.264 + `sowt`. No `repi`. Out of scope. |

Copied to `refs/`: 75 files, 47 modes, 26 GB. Ten modes have no original under 1.5 GB. They were not
copied. Their paths are in `inventory.tsv`.

## Previously unpaired `.DAT` files

Sixteen corpus `.DAT` files had no usable reference in their own folder.

| `.DAT` | Family | Reference that works | Result |
|---|---|---|---|
| `A_0001C809H241214_1422235Q` | C | R5 II `A_0026C730H250623_105158EJ` | **Repaired.** 1127 frames, 0 decode errors, audio = video, frames clean |
| `A_0002D054H241221_134042C0` | C | same | **Repaired.** 135 frames |
| `A_0015C453H250312_151308LJ` | C | same | **Repaired.** 20464 frames, 9.70 GB, audio 170.703875 s = video 170.703867 s |
| `A_0028C829H250627_1255235E` | C | same | **Repaired.** 12820 frames, 6.07 GB |
| `LAD33835` | A | R3 `LAD37341` (4K 29.97p, no audio) | **Repaired.** 16383 frames, 31.9 GB, last 6 s decode clean. Shot in portrait; no rotation flag, same as originals. |
| `LAD33743` | B (R3) | R3 `LAD33690` or `LAD33745` | **Blocked by issue 1.** Independent decode: 368/368 frames, 0 errors |
| `LAD10430`, `LAD11225`, `LAD11582`, `LAD11830`, `LAD11981`, `LAD11983`, `LAD12023`, `LAD12203`, `LAD18029`, `LAD18036` | B (R5) | R5 `LAD19893` or `LAD19894` (4K 59.94p ALL-I `twos`, `refs/R5/hvc1-4096x2160-59.940-ALL-I-42210b-twos2ch/`) | **Blocked by issue 1.** Independent decode: 578, 30, 303, 577, 578, 159, 337, 240, 63, 94 frames. All match the spec. 0 errors. Last frames clean |

The "independent decode" took the parameter sets from the reference and walked the `.DAT` with
the spec's family B layout (each frame padded to a multiple of 8, then 128 bytes × n of records).
It decoded the result with ffmpeg (`refs/scripts/famb_decode.py`). This proves the reference fits.
Only the engine is blocking.

So 5 of the 16 now repair. The other 11 will repair once issue 1 is fixed.

## Engine issues

### 1. Every family B `.DAT` is rejected (high)

**What.** Family B is a CAEP recording with LPCM audio: R3 at 59.94p with sound, and R5. The groups
per interval are `tmcd`, `lpcm`, `vide`. All 11 family B corpus files fail at "checking the
reference clip". The error is `temporal id is zero` or `frame does not start with its first slice
segment`. This happens even with `LAD33745.MP4`, the documented reference from the same shoot.

**Cause.** `repi.rs` pads each video sample to an **absolute file offset** (`align_up(off, align)`
in `Group::samples` and `detect_alignment`). Canon pads the **sample size** (spec §5: each sample
occupies `round_up(size, 8)` bytes). In family A the first frame starts at a multiple of 8, so both
rules agree. In family B the first frame starts at 4 mod 8: the 140-byte `vide` header follows a
`tmcd` group (140 B) and an `lpcm` group (96,116 B). Every candidate alignment then fails the NAL
chain check, and `detect_alignment` falls back to "packed" (1). From frame 2 on, every offset is
wrong.

**Example.** `LAD11225.DAT`: the first `vide` sample is at byte 96,412 (≡ 4 mod 8). With size-padding
all 30 frames start with a valid NAL header. With no padding only 1 of 30 does
(`refs/scripts/r5layout.py`). `datfix inspect` reports the gap as 3,912 bytes: 3,840 bytes of records
plus 72 bytes of padding it did not account for.

**User result.** No R5 or R3-with-audio `.DAT` can be repaired. The message tells the user to pick
another reference, but no reference will work.

**Fix.** Pad relative to the sample: `next = here + round_up(size, align)`. Apply this in
`Group::samples`, `Group::end_of` and `detect_alignment`.

**Check.** `datfix repair LAD33743.DAT --reference LAD33745.MP4` writes 368 frames with a clean
ffmpeg decode. Add a synthetic test whose first `vide` sample starts at 4 mod 8.

### 2. Creation date is the `.DAT` modification time (medium)

**What.** `repair.rs` sets `mvhd`/`tkhd`/`mdhd` creation time from the `.DAT` file's mtime. Canon
writes the recording **start** time in the camera's own time convention.

**Examples.**

1. `B_0004C512…123515LE`: the repaired file says 2024-11-09 17:36:58 UTC. The camera would write
   about 16:35:15. Its neighbour `B_0004C511…123428` says 16:34:28. The value is 1 h 1 m 43 s late:
   the clip's length, plus the gap between the host time zone and the camera's.
2. `LAD37315` (R3): the repaired file says 21:13:04. Its neighbour `LAD37316` started at 19:13:28.
3. `r5m2_64MB_repaired` was made from a copied `.DAT` with `cp`. It says 2026-11-26, the day of the
   copy. Resolve shows that date as "Date Created".

**User result.** Editors who sort or sync by date see the clip in the wrong place. Copying the
`.DAT` without keeping its timestamps makes the date meaningless.

**Fix.** Subtract the recovered duration from the mtime. Then shift by the reference's offset:
reference `mvhd` time minus (reference mtime − reference duration). For R5 II, the file name
holds the start time (`H241109_123515`), and the `tmcd` start agrees with it. Prefer that when it
parses.

**Check.** Repairing `B_0004C512…123515LE` gives `CreateDate` 16:35:15 ± 2 s.

### 3. Canon `uuid` and `udta` are copied verbatim from the reference (medium)

**What.** The output has the reference's Canon `uuid` box. That box holds an embedded JPEG
thumbnail and EXIF data: capture date, firmware, lens and serial fields. GPS in `udta` comes from the
reference too.

**Examples.**

1. `B_0004C512_123515LE_repaired.MP4` has EXIF date 2024:11:09 12:34:28 and firmware 1.0.1. Those
   belong to the reference `B_0004C511`.
2. `r5m2_64MB_repaired.MP4` is a December 2024 recording. It has EXIF date 2026:11:21 20:58:18 and
   firmware 1.3.1, from the wedding reference `A_0071C743…`.
3. `LAD37315_repaired.MP4` has GPS 42.14262, −124.35695 from `LAD37314`. The original `LAD37341`
   has none.

**User result.** Finder, Canon software and DAMs show another clip's thumbnail and date. A user
who repairs with a reference from another shoot gets wrong location and date metadata.

**Fix.** Keep the box, because some apps want it. Replace the thumbnail with a frame from the
repaired clip, or drop it. Rewrite or remove the EXIF date fields. Drop GPS unless the reference
comes from the same folder. Say in the CLI output which metadata came from the reference.

**Check.** `exiftool -a -G1 B_0004C512_123515LE_repaired.MP4` shows no 12:34:28 timestamp.

### 4. Reference-check errors point at the wrong thing (low)

`temporal id is zero` and `frame does not start with its first slice segment` are parser details.
Under issue 1 they appear with the correct reference. When frame 0 parses and a later frame does
not, the cause is the sample layout, not the reference. The message should say so. It should not
ask the user to pick another clip.

### 5. Notes, not bugs

- `--reference-dir` is not recursive. Many working references are in a sibling folder
  (`100EOSR3/`) or another shoot. A "search the NAS/card for a matching original" helper would close
  most of the 16 cases without a human.
- The `.CRM` files need a model string from somewhere other than `udta/modl`. Their size suggests
  R3 (6000×3164) and R5 (8192×4320).
