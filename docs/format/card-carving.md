# Card carving: recovering recordings the file system lost

Status: implemented in `datfix scan` (core module `carve`), tested on synthetic exFAT and FAT32
card images with corpus `.DAT` files (2026-11-26).

## 1. The problem

A Canon camera writes an interrupted recording into a `.DAT` file (see
[canon-dat-format.md](canon-dat-format.md)). Sometimes the file system does not show that data:

- The `.DAT` entry exists but has **0 bytes**. The camera allocated and wrote clusters but never
  updated the directory entry.
- The entry is **missing**. On FAT32 cards an interrupted recording can leave no entry at all
  (horshack, EOSHD thread).
- The entry was **deleted** (by the user, a computer, or a camera "format" that only rewrote the
  file tables).

In all three cases the recording's bytes are usually still on the card. `datfix repair` cannot
help, because it needs a file. `datfix scan` reads the whole card and finds the recordings by
their content.

## 2. Safe usage

The scanner only reads. It opens the image or device read-only and never writes to it. Even so,
**image the card first** and scan the image:

1. Stop using the card. Do not record, format or "repair" it in the camera or the computer.
2. Make a raw image.
   - macOS Disk Utility: select the card device (not the volume), *File > New Image > Image
     from "…"*, format **DVD/CD master** (raw `.cdr`) or **read/write** (raw `.dmg`).
     Compressed images do not work.
   - macOS command line: `diskutil list` to find the card (for example `disk4`), then
     `diskutil unmountDisk disk4` and `sudo dd if=/dev/rdisk4 of=card.img bs=4m status=progress`.
   - Linux: `sudo dd if=/dev/sdX of=card.img bs=4M status=progress conv=noerror,sync`. For a
     failing card use `ddrescue`.
   - Windows: a raw imaging tool (for example the `dd` in Git Bash, or HDD Raw Copy). Raw
     device paths are `\\.\PhysicalDriveN`.
3. `datfix scan card.img` to see what is there. Add `-o <folder>` to save, `--repair` to repair.

Scanning the device directly (`/dev/rdisk4`, `/dev/sdX`, `\\.\PhysicalDriveN`) also works and
needs administrator rights. It is read-only, but an image protects against a card that dies
during a second read.

The desktop app runs the same scan ("Recover from a memory card"); see the user guide.

```text
datfix scan card.img                       # report only
datfix scan card.img -o recovered          # save each recording as <card name>.DAT or recovered_<offset>.DAT
datfix scan card.img -o recovered --repair # also write the repaired .MP4 next to each .DAT
           [--clips] [--fragments] [--no-stitch] [--json]
```

- `--repair` uses finished clips found on the same card as references (same brand, nearest on
  the card first, up to 3, copied to `<folder>/clips/`), then the built-in profiles.
- `--clips` saves every finished Canon clip found, including deleted ones.
- `--fragments` saves headerless pieces (section 5) as `fragment_<offset>.DAT`.
- `--json` prints newline-delimited events (`phase`, `progress`, `found`, `result`) like
  `repair`.

`<offset>` is the byte offset in the image where the recording starts.

## 3. How it works

### 3.1 Signature pass

One sequential read of the whole image in 8 MiB blocks. A reader thread keeps the next block
loading while the previous one is searched (SIMD substring search). Three signatures are
recorded:

| Signature | Where | Meaning |
|---|---|---|
| `00000000 'mdat' 00000008 'wide'` | sector-aligned | start of a `.DAT` (a file starts on a cluster) |
| `'ftyp'` box with brand `CAEP` or `XFVC` | sector-aligned | start of a finished Canon MP4 |
| `repi` + valid 20-byte header | anywhere | one group of samples |

A group header counts only when the flag is `0x83`, the type is one of `vide tmcd lpcm aac
tdmt`, the sample count is 1–8,000,000 and `header_len` agrees with the size table. This is the
only full read; everything after it uses small random reads. Speed is the disk's speed
(section 7).

### 3.2 File system (optional)

If the image has a FAT-family file system (at offset 0, or in an MBR or GPT partition), the
scanner reads its boot sector, FAT and directories. It never needs them, but they add:

- **Names**: a recording whose start is where a `.DAT` entry's first cluster points gets that
  entry's name, live or deleted (`file_match: first_cluster`). A 0-byte entry has no first
  cluster, so it is matched by time instead (`file_match: start_time`): the time in its Canon
  name (`…H241221_134042…`), or else its creation time, must equal the time of day of the
  recording's first timecode counter within 2 s. Only a match that is unique both ways counts;
  two candidates on either side leave the recording unnamed.
- **Times**: the entry's creation and modification times (exFAT adds a time zone).
  `carve::hint_for` passes them, with the original name, to the repair
  (`RepairOptions::hint`), so the repaired file is not dated by the scan.
- **Hints**: every `.DAT` entry is listed with what was found at its first cluster. exFAT keeps
  the first cluster in deleted entries. FAT32 keeps it too, but a deleted short name loses its
  first letter (shown as `_`). A 0-byte entry usually has no first cluster.
- **Cluster size** for stitching (section 4).
- **File size**: a live entry with a non-zero size sets the recording's end.

exFAT and FAT32 only. NTFS, HFS+ and APFS are not parsed (cameras do not use them).

### 3.3 Finished clips

Each Canon `ftyp` is parsed box by box (`ftyp`, `moov`, `uuid`, `free`, `mdat`) to find the
clip's extent, its `mdat` range, the camera (`udta/modl`) and the duration (`mvhd`). The `repi`
groups inside a finished clip's `mdat` belong to that clip: they are never reported as a
recording or a fragment.

### 3.4 Following a chain

From each `.DAT` start, the group at offset 16 is the first group. The next group must start
right after the samples of the current one:

- `tmcd`, `lpcm`, `aac `, `tdmt`: exactly at the end of the samples.
- `vide`: after the samples plus padding (0–7 bytes per sample) plus, for bodies that write
  them, 128 bytes of per-frame records per frame. The padding and record layout is learned
  from the first video group and then enforced.

If no header is at the exact place, the next header within 64 KiB is accepted when its type was
already seen in this recording (a resync; counted in the report).

A `tmcd` group must continue the timecode exactly: its first counter must equal the previous
`tmcd` group's first counter plus its frame count. This stops a chain from running into an old
recording whose data happens to follow on the card.

### 3.5 Where a recording ends

A camera `.DAT` is always a whole number of 32 MiB units, and it ends inside its last group
(spec section 7). So the recovered `.DAT` ends at the first 32 MiB boundary (counted from the
`.DAT` start) after the last group header found. This reproduces the camera's file byte for
byte. Exceptions:

| End | When |
|---|---|
| `dat_unit` | the normal case |
| `file_size` | a live directory entry gives a size that fits the last group |
| `last_group` | no 32 MiB boundary fits (not a camera `.DAT` size, or a fragment); trailing video samples whose NAL lengths do not add up are dropped |
| `image_end` | the image ends first |

## 4. Fragmented recordings (stitching)

Camera recordings are usually contiguous. When a chain breaks inside the image, the scanner
looks for a continuation elsewhere. It joins two pieces only when all of these hold:

1. The recording has a `tmcd` stream.
2. A group header elsewhere (not used by another recording, not inside a finished clip) is a
   whole number of clusters away from where the next header should be. Cluster = the file
   system's cluster size, or 4 KiB without a file system.
3. Following that header, the first `tmcd` group continues the timecode exactly.
4. Exactly one such place exists. Two or more is ambiguous: no join, the recording is marked
   partial.
5. The break point is proven. Candidates are boundaries between the end of the broken group's
   header and the expected next header, first on 32 MiB units counted from the `.DAT` start
   (SD cards allocate in such units), then on cluster boundaries (the file system's, or
   512 KiB down to 4 KiB). A candidate passes when every sample of the broken group checks out
   through the joined view: NAL lengths add up for video, records start with `0x04` and the
   same body code, timecode counters are consecutive. Exactly one candidate at the first level
   that has any must pass.

Up to 64 joins per recording. The report lists each piece (`offset+length`).

**Video-only recordings** (R3 without audio, family A) have no timecode. They are never joined.
If a piece elsewhere continues the 7-bit per-frame record counter, the recording is marked
partial with that piece's offset, and the piece is listed as a fragment.

## 5. Fragments

Chains that no `.DAT` start leads to, outside finished clips, with at least 2 groups, are listed
as headerless fragments: the tail of a split recording, or an old recording whose start was
overwritten. `--fragments` saves them with a 16-byte `.DAT` header added, so `datfix repair` can
read them.

## 6. Limits

- A deleted or 0-byte recording is recoverable only while its clusters have not been reused.
  A recording that overlaps a finished clip is flagged (`overlaps_clips`): part of it was
  overwritten.
- A chain that breaks inside a video frame made of one NAL unit keeps that frame, because only
  the NAL length prefix can be checked. At most one damaged frame remains (the repair keeps it;
  it may show artifacts).
- Family D bodies (EOS R, M50 II) write no `repi` index. Their recordings are not found.
- Stitching needs the camera's write order: pieces in recording order, each starting on a cluster.
  Recordings broken into more than 64 pieces stop at the 64th break.
- Compressed disk images (`UDZO`, `.E01`, sparse bundles) are not read. Convert to raw first
  (`hdiutil convert -format UDTO in.dmg -o out`).
- A recording with no matching entry (no first cluster, and no unique start-time match) is named
  `recovered_<offset>.DAT`. Its repaired file gets no recording date ("unknown"), because the
  copy's own file time is the time of the scan and is never used; the timecode track still
  gives the time of day. A timecode preset to a value other than the clock defeats the
  start-time match.

## 7. Performance

| Test | Result |
|---|---|
| 2 GB exFAT image file | 0.8 s (2.8 GB/s, partly cached) |
| same image as `/dev/rdisk6` (read-only attached) | 3.0 s (0.7 GB/s) |
| 37.8 GB R5 Mark II `.DAT` scanned as an image | 14.3 s (2.6 GB/s); 9,303 groups and 79,727 frames, as in the format spec |

Memory: one entry per group header (about 40 bytes; a full 512 GB card holds well under a million
headers) plus three 8 MiB blocks.

## 8. Tests

- Unit tests (`carve::fs`, `carve::source`, `carve` tests): exFAT live/deleted/0-byte entries,
  exFAT in an MBR partition, FAT32 long/short/deleted names and geometry, stream views; synthetic
  card images: recording cut at a 32 MiB unit, stitching with timecode proof, no stitching
  without timecode, a stale recording after a live one, finished clips, a deleted exFAT entry,
  two equal continuations (ambiguous), a signature split across read blocks, cancel.
- Card images built with `hdiutil create -size 2g -fs ExFAT` and `-fs "MS-DOS FAT32"`: a corpus
  `.DAT` copied twice, one copy deleted and one truncated to 0 bytes, next to healthy clips.
  Both recovered `.DAT`s are byte-identical (SHA-256) to the original on both file systems;
  `--repair` output decodes with no ffmpeg errors and has as many frames as the index.
- Fragmented images from `tools/make_fragmented_image.sh` (a `.DAT` split at 256 MiB, and at
  100 MiB + 384 KiB, with random data between): the joined `.DAT` is byte-identical to the
  original. A video-only R3 `.DAT` split the same way is not joined and is reported partial with
  the continuation's offset.
