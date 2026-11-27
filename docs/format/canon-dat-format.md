# Canon `.DAT` interrupted-recording format

Status: verified on 36 `.DAT` files and about 1,000 healthy Canon MP4s in a local test corpus
(2026-11-26). Corpus files are named by neutral ids (`R3-01`, `R5-01`, `R5II-01`). The research
scripts are not in git.

## 1. Background

A Canon camera records a clip in two parts:

- The **payload**: the video frames, audio and per-frame data. The camera writes it while it records.
- The **header**: `ftyp` (file type) and `moov` (the index of every sample). The camera writes it
  only when recording stops.

While the camera records, the payload goes to a temporary file with the `.DAT` extension. When
recording stops normally, the camera adds the header and the file becomes an `.MP4`. When power or
the card fails, the header is never written. The `.DAT` stays on the card. No player can open it.

A repair tool reads the `.DAT`, reads one healthy `.MP4` from the same camera with the same settings,
and writes a new `.MP4` with a rebuilt header.

## 2. Key result: a `.DAT` is the `mdat` of the finished file

The `.DAT` is byte-for-byte the `mdat` box of the MP4 that the camera would have written. Proof:

- Every healthy Canon MP4 whose body uses `repi` has a `repi` header as the first bytes of its `mdat`
  payload, in the same group pattern as the `.DAT` files from the same body. Example:
  `100EOSR3/LAD33745.MP4` (R3), mdat payload at 2097168 starts `repi … tmcd`, then `lpcm`, then `vide`,
  exactly like `R3-11.DAT` from the same shoot.
- Every chunk offset in a healthy `moov` points at a sample inside a `repi` group. Bytes between
  chunks are always a `repi` header, padding, or per-frame metadata records (section 5).

The `.DAT` starts with a 16-byte `mdat` placeholder. The finished file replaces it with a 64-bit
`mdat` header of the same length. So **DAT offset X = (mdat box start in the MP4) + X**.

```
.DAT offset 0:   00 00 00 00 'm d a t'  00 00 00 08 'w i d e'     <- placeholder (size 0, then an 8-byte 'wide' box)
.DAT offset 16:  'r e p i' ...                                    <- first group, always at 16
Finished MP4:    00 00 00 01 'm d a t'  <u64 size>                <- same 16 bytes, rewritten at finalize
```

All 36 corpus `.DAT` files start like this. None starts with a header placeholder.

## 3. The `repi` group header

All fields little-endian.

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 4 | magic | `repi` |
| 4 | 3 | header_len | u24. `20` when fixed_size ≠ 0, else `20 + 4·n` |
| 7 | 1 | flag | always `0x83` |
| 8 | 4 | type | FourCC, see table below |
| 12 | 4 | fixed_size | size of every sample, or 0 |
| 16 | 4 | n | sample count in this group |
| 20 | 4·n | sizes[n] | only when fixed_size = 0 |

The samples follow the header. Section 5 gives the exact spacing rules.

### Type codes seen

| `repi` type | Meaning | MP4 track (sample entry) | Sizes |
|---|---|---|---|
| `vide` | video frames | `hvc1` or `avc1` | per-sample table |
| `tmcd` | timecode, one u32 BE frame counter per video frame | `tmcd` | fixed 4 |
| `lpcm` | linear PCM | `twos` (CAEP) or `ipcm` (XFVC) | fixed (see family) |
| `aac ` | AAC frames (note trailing space) | `mp4a` | per-sample table |
| `tdmt` | Canon per-frame metadata | `cndm` (handler `meta`) | fixed 717 (R5 II), 1476/1484 (C80) |

No other type appears in the corpus or in the healthy files we walked.

## 4. Body families

Body names come from the `udta/modl` string of healthy MP4s, and from a body code in the per-frame
metadata record (section 6), which matches `modl` on every healthy file we checked.

| Family | ftyp brands | Bodies | Group pattern per interval | Video sample padding | Per-frame record |
|---|---|---|---|---|---|
| **A** CAEP, video only | `mp42 mp42 hvc1 CAEP` | R3 (audio off) | `vide×F` | round up to 8 | 128 B × F |
| **B** CAEP + LPCM | `mp42 mp42 hvc1 CAEP` | R3, R5, R6 II | `tmcd×F`, `lpcm×1`, `vide×F` | round up to 8 | 128 B × F |
| **B′** CAEP + AAC | `mp42 mp42 {hvc1,avc1} CAEP` | R5, R6, R6 II | `tmcd×F`, `aac ×23–24`, `vide×F` | round up to 8 | 128 B × F |
| **B″** CAEP cinema | `mp42 mp42 hvc1 CAEP` | C70 | `tmcd×F`, `aac ×23–24`, `vide×F` | none | none |
| **C** XFVC + LPCM | `XFVC XFVC mp42 iso2` | R5 II, C80 | `tmcd×F`, `tdmt×F`, `lpcm×24024` ×4, `vide×F` | none | 128 B × F |
| **C′** XFVC + AAC | `XFVC XFVC mp42 iso2` | C80 | `tmcd×F`, `tdmt×F`, `aac ×23–24`, `vide×F` | none | 128 B × F |
| **D** no index | `mp42 mp42 avc1 CAEP` | EOS R, M50 II, two unnamed avc1 bodies | no `repi` in mdat | — | — |

F is the frame count of one interval. One interval is 0.5005 s (24024 audio samples at 48 kHz):
F = 15 at 29.97p, 30 at 59.94p, 60 at 119.88p, 12 at 23.976p. The last group before a normal stop
can be shorter (`R5-07.DAT` has a 16-frame group before its truncated tail).

Families B′, B″, C′ and D are **not** in the `.DAT` corpus. Their layout comes from healthy MP4s only.

## 5. Byte layout of one interval

### Family A — R3, no audio (29.97p)

```
repi vide n=15 (80 B)
  frame 0 | pad0 | frame 1 | pad1 | … | frame 14 | pad14        each pad = (-size) mod 8, 0–7 bytes
  record 0 | record 1 | … | record 14                           15 × 128 = 1920 B, not indexed
repi vide n=15 …
```

Evidence: `LAD39664.MP4` chunk 15 ends at 32750989. 3 pad bytes. Records 32750992–32752911.
Next `repi` at 32752912. Next chunk at 32752992 (= 32752912 + 80).

### Family B — R3 / R5 with LPCM (59.94p)

```
repi tmcd fixed=4 n=30 (20 B)     120 B: 30 × u32 BE frame counters
repi lpcm fixed=96096 n=1 (20 B)  96096 B: 24024 stereo frames, 16-bit big-endian, L R interleaved
repi vide n=30 (140 B)            30 frames, each padded up to a multiple of 8
                                  30 × 128 = 3840 B records, not indexed
```

The MP4 stores the LPCM blob as track `twos`, 2 ch, 16 bit, `stsz` fixed 4, 24024 samples per chunk.
So one `repi` sample of 96096 bytes becomes 24024 MP4 samples of 4 bytes.
Evidence: `100EOSR3/LAD33745.MP4`: tmcd chunk at 2097188 (120 B), twos chunk at 2097328 (96096 B),
first hvc1 sample at 2193564 = 2097328 + 96096 + 140.

### Family C — R5 Mark II (119.88p in all 15 corpus DATs)

```
repi tmcd fixed=4 n=60       (20 B)    240 B
repi tdmt fixed=717 n=60     (20 B)    43020 B          -> cndm track
repi lpcm fixed=3 n=24024    (20 B)    72072 B          -> ipcm track 1 (mono, 24-bit)
repi lpcm fixed=3 n=24024    (20 B)    72072 B          -> ipcm track 2
repi lpcm fixed=3 n=24024    (20 B)    72072 B          -> ipcm track 3
repi lpcm fixed=3 n=24024    (20 B)    72072 B          -> ipcm track 4
repi vide n=60               (260 B)   60 frames back to back, no padding
                                       60 × 128 = 7680 B records, not indexed
```

The four `lpcm` groups map to the four `ipcm` tracks in order of appearance.
Evidence: `A_0053C606H260110_114719SP_LAD04.MP4`: chunks at 2097188 (tmcd), 2097448 (cndm),
2140488/2212580/2284672/2356764 (ipcm 1–4), 2429096 (hvc1); hvc1 chunk is followed by a 7700-byte gap
(7680 records + 20-byte `tmcd` header). At 59.94p (`B_0004C468H241109_111632FE_LAD04.MP4`) F = 30
and the record block is 3840 B.

### Padding rule (exact)

- Families A, B, B′: each `vide` sample occupies `round_up(size, 8)` bytes. The rule is on the size,
  not the absolute offset. Proof: in `LAD33745.MP4` every video sample starts at offset ≡ 4 (mod 8).
- Pad bytes are stale buffer data (for example `0004bbd926`), not zeros. Ignore them.
- Families B″, C, C′: no padding.
- `tmcd`, `lpcm`, `aac `, `tdmt` samples are never padded. The next `repi` follows the last sample directly.

The healthy MP4 handles the padding with one chunk per video sample (`stsc` = 1 sample per chunk).
A repair tool must do the same for A/B/B′. For C it may use one chunk per group, like the camera.

## 6. The unindexed bytes (Q3)

The ~1967–1994-byte gaps in `r3_128MB.DAT` are:

1. The sum of the 15 per-frame pads (0–7 bytes each, so about 0–105 bytes in total). A scanner that
   adds the unpadded sizes stops short by this amount.
2. Then **15 records of 128 bytes = 1920 bytes**, one per video frame, after the last frame.

They are not audio. They are not referenced by any track in the healthy `moov`. The camera writes them
but drops them from the index. The tail bytes in the brief (`… 45 25 af 00 1d 00 1a 00 02 02 60 cc cc …`)
are the end of record 14 of a group.

Record layout (partly decoded):

| Bytes | Content |
|---|---|
| 0 | `04` |
| 1–3 | body code: R3 `00 00 45`, R5 `00 10 42`, R6 `00 30 45`, R6 II `00 10 48`, R5 II `00 60 49` |
| 0x0f–0x10 | counter that steps by −2 per frame |
| 0x3c–0x3e | frame counter, +1 per frame |
| rest | exposure/lens-like data, not needed |

How to size them deterministically: **records = n × 128 bytes, placed right after the last padded
video sample of a `vide` group**, for every family except B″ (C70) and D. Then the next `repi` must
start. A tool should assert this, and fall back to a forward scan for `repi` + `0x83` (max 64 KiB)
only when the assertion fails.

## 7. Truncated tail and other quirks

- **Every `.DAT` size is an exact multiple of 32 MiB.** The camera writes in 32 MiB units.
- **Every `.DAT` ends inside a `vide` group.** The group header and its size table are on disk, but
  the frames run past EOF. No `.DAT` has trailing zero bytes.
- The frames of that last group that fit on disk are real data. In all 36 files, each complete frame
  in the truncated group starts with a valid length-prefixed NAL header. Keep them; drop the first
  frame that crosses EOF. Recoverable frames range from 0 to 57 per file.
- The audio, `tmcd` and `tdmt` groups of the truncated interval are complete (they precede `vide`).
  Trim audio to the recovered video duration: audio samples = frames × 48000 × 1001 / (fps × 1000).
- Groups can be shorter than F at the end of a recording (`R5-07.DAT`: `tmcd×16`, `lpcm 51248`,
  `vide×16`).
- **No 4 GB splitting.** Bodies write one file (largest `.DAT` 35.2 GiB; healthy MP4s use `co64`).
- **Header reservation** varies: mdat payload starts at 2097168 (2 MiB) in R3/R5 II, 2621456
  (2.5 MiB) for long R5 II/R6 II clips, and 262160–1572880 for R5/R6/C70/C80. The space is filled by
  `moov` itself or by a `free` box (`LAD39664.MP4`: `free` 2012954 B). It does not affect the payload.
  A repair tool can use any `mdat` offset, because padding is relative to sample size.
- R3 ALL-I video has no `stss`/`ctts` (every frame is a sync frame). R5, R6 II and R5 II use Long GOP
  and need `stss` and `ctts`.
- No video sample carries VPS/SPS/PPS. The reference MP4 must supply `hvcC`/`avcC`.

## 8. Track-mapping rules for the tool

1. Read `ftyp` of the reference. `CAEP` → families A/B/B′/B″. `XFVC` → C/C′.
2. Walk groups from DAT offset 16. Validate magic, flag `0x83` and `header_len`.
3. Map types to reference tracks by handler and FourCC:
   `vide`→`hvc1`/`avc1`; `tmcd`→`tmcd`; `tdmt`→`cndm`; `aac `→`mp4a`;
   `lpcm`→`twos` (one track) or the k-th `ipcm` track for the k-th `lpcm` group in an interval.
4. Choose the video padding mode from the reference: if its video `stsc` is 1 sample per chunk and
   consecutive chunks differ by `round_up(size, 8)`, use pad-8. Else no padding.
5. After each `vide` group, skip `n × 128` bytes when the reference has record gaps (gap between the
   last video chunk of a group and the next chunk = `n × 128 + next header_len`). C70 has none.
6. Sample tables:
   - `vide`: sizes from `repi`; offsets from the padding rule; one chunk per sample (pad-8) or per group.
   - `twos`: `stsz` fixed = channels × bytes per sample (4); samples per group = blob / 4.
   - `ipcm`, `tmcd`, `tdmt`: `stsz` fixed from `repi`; one chunk per group.
   - `aac `: sizes from `repi`; one chunk per group.
7. Refuse when the reference and the DAT disagree on: type set, F per interval, `lpcm` group count,
   fixed sizes (`tdmt` 717 vs 1476), or padding mode.

## 9. Corpus by family (Q5)

| Family | Body | Files | Total size |
|---|---|---|---|
| A CAEP video only | R3 | 10 | 54.7 GiB |
| B CAEP + LPCM | R5 ×10, R3 ×1 | 11 | 6.5 GiB |
| C XFVC | R5 Mark II | 15 | 66.0 GiB |

Recovered frames = complete groups plus complete frames of the truncated group.

| Family | Body | Id | Size (GiB) | Groups | F | Frames | Seconds |
|---|---|---|---|---|---|---|---|
| A | R3 | R3-01.DAT | 29.69 | 1093 | 15 | 16383 | 546.6 |
| A | R3 | R3-02.DAT | 6.00 | 221 | 15 | 3303 | 110.2 |
| A | R3 | R3-03.DAT | 0.34 | 13 | 15 | 188 | 6.3 |
| A | R3 | R3-04.DAT | 0.25 | 10 | 15 | 136 | 4.5 |
| A | R3 | R3-05.DAT | 0.56 | 21 | 15 | 308 | 10.3 |
| A | R3 | R3-06.DAT | 1.66 | 61 | 15 | 910 | 30.4 |
| A | R3 | R3-07.DAT | 2.31 | 85 | 15 | 1269 | 42.3 |
| A | R3 | R3-08.DAT | 1.62 | 61 | 15 | 901 | 30.1 |
| A | R3 | R3-09.DAT | 12.12 | 445 | 15 | 6674 | 222.7 |
| A | R3 | R3-10.DAT | 0.12 | 5 | 15 | 67 | 2.2 |
| B | R3 | R3-11.DAT | 0.72 | 39 | 30 | 368 | 6.1 |
| B | R5 | R5-01.DAT | 1.12 | 60 | 30 | 578 | 9.6 |
| B | R5 | R5-02.DAT | 0.06 | 6 | 30 | 30 | 0.5 |
| B | R5 | R5-03.DAT | 0.59 | 33 | 30 | 303 | 5.1 |
| B | R5 | R5-04.DAT | 1.12 | 60 | 30 | 577 | 9.6 |
| B | R5 | R5-05.DAT | 1.12 | 60 | 30 | 578 | 9.6 |
| B | R5 | R5-06.DAT | 0.31 | 18 | 30 | 159 | 2.7 |
| B | R5 | R5-07.DAT | 0.66 | 36 | 30 | 337 | 5.6 |
| B | R5 | R5-08.DAT | 0.47 | 27 | 30 | 240 | 4.0 |
| B | R5 | R5-09.DAT | 0.12 | 9 | 30 | 63 | 1.1 |
| B | R5 | R5-10.DAT | 0.19 | 12 | 30 | 94 | 1.6 |
| C | R5 II | R5II-01.DAT | 0.50 | 133 | 60 | 1127 | 9.4 |
| C | R5 II | R5II-02.DAT | 0.06 | 21 | 60 | 135 | 1.1 |
| C | R5 II | R5II-03.DAT | 9.03 | 2394 | 60 | 20464 | 170.7 |
| C | R5 II | R5II-04.DAT | 1.47 | 392 | 60 | 3321 | 27.7 |
| C | R5 II | R5II-05.DAT | 0.81 | 217 | 60 | 1833 | 15.3 |
| C | R5 II | R5II-06.DAT | 1.41 | 371 | 60 | 3177 | 26.5 |
| C | R5 II | R5II-07.DAT | 5.66 | 1498 | 60 | 12820 | 106.9 |
| C | R5 II | R5II-08.DAT | 35.19 | 9303 | 60 | 79727 | 665.1 |
| C | R5 II | R5II-09.DAT | 0.47 | 126 | 60 | 1057 | 8.8 |
| C | R5 II | R5II-10.DAT | 0.69 | 182 | 60 | 1555 | 13.0 |
| C | R5 II | R5II-11.DAT | 0.91 | 245 | 60 | 2045 | 17.1 |
| C | R5 II | R5II-12.DAT | 1.53 | 406 | 60 | 3461 | 28.9 |
| C | R5 II | R5II-13.DAT | 5.44 | 1442 | 60 | 12303 | 102.6 |
| C | R5 II | R5II-14.DAT | 2.66 | 707 | 60 | 6004 | 50.1 |
| C | R5 II | R5II-15.DAT | 0.19 | 56 | 60 | 420 | 3.5 |

All 36 walk cleanly from offset 16 to the truncated last group with zero resyncs.

## 10. Reference availability

A repair needs a healthy original MP4 with the same codec settings. Many neighbouring files in the
corpus folders are **re-encodes, not originals** (brand `isom/av01` via `av1_qsv`, or `isom hev1`).
They carry no Canon `hvcC` and cannot be used.

| DATs | Usable reference |
|---|---|
| Family A (all 10) | `100EOSR3/*.MP4` in the same folder (R3, 4096×2160 29.97p, video only) |
| `R3-11.DAT` (R3 + LPCM) | an R3 59.94p + LPCM original from the same shoot (`100EOSR3/LAD33745.MP4`) |
| Family B R5 (all 10) | **none in folder** (AV1/hev1 re-encodes). Need an original R5 HEVC 59.94p + LPCM clip. The re-encodes say 4096×2160; the DAT's first 8 frames of each group checked are all IDR (NAL type 19), so probably ALL-I. No such original was found in the corpus (the 49 R5 HEVC 59.94p originals all use AAC and Long GOP) |
| Family C (13) | neighbouring `*_LAD04.MP4` (R5 II), but check F: some neighbours are 59.94p while the DAT is 119.88p |
| `R5II-01`, `R5II-02` | neighbours are AV1 re-encodes; use any original R5 II 4K 119.88p clip |
| `R5II-03`, `R5II-07` | no MP4 in folder; same as above |
