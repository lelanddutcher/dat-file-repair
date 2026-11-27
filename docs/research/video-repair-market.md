# Video file repair: vendor failure modes, market, gaps and a product sketch

Research date: 2026-11-26. Scope: everything except Canon `.DAT` (covered in `docs/research/existing-tools.md`).
Marks: **[V]** = verified from a primary source (manufacturer manual, repo, vendor price page).
**[U]** = user/forum report. **[?]** = uncertain, vendor marketing, or my inference.

## 0. The one-paragraph answer

Almost every "corrupt video" from a camera, phone, drone or recorder is the same failure: the
camera writes audio/video frames continuously and writes the **index** (MP4/MOV `moov` box, MXF
footer/index table, BRAW/ProRes `moov`) only when recording stops. Power loss, card pull or crash
leaves intact frames and no index. The fix is to rebuild the index — no re-encoding. Free
tools for this exist (untrunc, ffmpeg) but are command-line, need a reference clip, and fail on
modern pro formats (10-bit 4:2:2 HEVC, PCM audio, metadata tracks, MXF). Paid tools fill the gap
with a preview-then-pay or subscription model. Several manufacturers ship in-camera recovery that
users don't know about. A general free tool that (1) tells users to try the in-camera fix first,
(2) recognises the vendor/mode, (3) rebuilds the index without a reference where possible, and
(4) validates the output honestly, has a real, under-served audience.

---

## 1. Sony

| Item | Finding | Mark |
|---|---|---|
| Interrupted file | Mirrorless/FX bodies leave a **`.RSV`** file (no index) in the clip folder instead of `.MP4`/`.MXF`. Frames are present. | [V] rsv.repair docs; [U] many |
| Variants | XAVC S / HS long-GOP MP4 (H.264/HEVC + LPCM `twos` + `rtmd` metadata track), XAVC S-I, XAVC MXF (FX6/FX9/Venice), X-OCN MXF. rsv.repair auto-detects "raw GOP, XAVC-I/MXF, X-OCN". | [V] |
| `rtmd` | Real-time metadata track (lens, gyro, GPS), header prefix `00 1c 01 00`. Generic untrunc misclassifies it: issue #137 found only 48 `rtmd` packets and 0 video. `-s` (step) mode works around it (Jeff Geerling, 2025). | [V] |
| "Recover Image DB" (mirrorless) | Rebuilds the **database file** on the card, not clips. Sony documents it for DB damage caused by computers/other devices; it does not delete images; fails if the battery is very low. **No Sony document says it repairs an interrupted movie.** Vendor blogs claim it can convert `.RSV` to `.MP4` [?]. One FS100 user lost all clips after a DB repair [U]. | [V]/[?] |
| "Restore Media" (pro camcorders: FX6/FX9/F55/PMW) | Camera writes a salvage file during recording; on next power-up it offers Restore Media and closes the interrupted clip. Usually only the buffer is lost. **Must be done in-camera before offload** — after copying to a computer the restore is no longer possible (Alister Chapman). Some users lose the last clip. | [V] FX9 quick ref; [U] Creative COW, DVXuser, xdcam-user |
| Playback problems that are not corruption | XAVC HS 10-bit 4:2:2 HEVC needs hardware decode (Apple M1 Pro+, recent Intel QSV, NVIDIA RTX 50); otherwise Premiere/VLC stutter or won't import; Resolve Free on Windows can't hardware-decode it. A repair tool should **diagnose and say "your file is fine, your decoder isn't"**. | [V]/[U] Adobe forums |
| Free tools | **rsv.repair** (OTTOMATIC, browser-local) launched free, **now pay-per-file after a short free preview** [V]; **macmixing/repair-rsv** (GPL-3, Tauri, patched untrunc `-rsv`, needs reference, ★8) [V]; raiz0tsky fork (Windows); RSV SOS (hansley.fr, untrunc+ffmpeg); jasonnovak/mp4-reference-repair (MIT, bundles recover_mp4 for XAVC). | [V] |
| Paid | footage.repair (claims no reference needed), rsvrepair.com (service), fix.video, Repairit, EaseUS, SnapLab, Restore.Media (MXF header/footer). | [V] listings / [?] claims |
| Catalyst Browse | No documented repair feature found. | [?] |

## 2. Panasonic

| Item | Finding | Mark |
|---|---|---|
| Interrupted file | Leaves **`.MDT`** (or a 0 KB file) instead of `.MOV`/`.MP4`. | [V] manuals, Aeroquartet |
| In-camera **[Video Repair]** | GH6, GH7, S5II (added by firmware), S5IIX, G9II [?]: Playback → Edit Image → Video Repair. **MOV and Apple ProRes only; MP4 cannot be repaired.** Short clips may fail; needs card free space; must be same camera; repaired file saved alongside, original kept. | [V] GH6, GH7, S5II, S5IIX manuals |
| Not covered | GH5/GH5S/G9/S1/S5 (original): no in-camera repair found [?]. Any MP4-mode recording. 0 KB files (needs card carving). | |
| Causes | Aeroquartet: >80% of GH5 failures traced to the vertical battery grip (hot-swap cuts power) [vendor claim]. Third-party batteries cutting off without warning (Fstoppers). | [?]/[U] |
| Tools | untrunc (reference), Grau (demo), Aeroquartet Treasured (service), fix.video (lists GH6/GH7/S5II/S9). | |

## 3. Blackmagic and Atomos

| Item | Finding | Mark |
|---|---|---|
| BRAW / ProRes (Pocket 4K/6K, URSA) | `.braw`/`.mov` with no `moov`. Users report the camera flushes every few seconds so little is lost; "some firmware" finalises on re-insert [U, unverified which]. BM support's own advice: graft a header from a same-settings clip. | [U] BM forum t=146540, 175161 |
| Video Assist / HyperDeck | No documented auto-recovery found. Dual batteries for hot-swap. | [?] |
| BRAW tools | brawrecovery.se (hybrid-local tool, needs reference, paid), brawrecovery.com (manual service), Aeroquartet, FixMyVideos, Gillware (lab). One user paid ~$200 for one 30-min BRAW. **No free BRAW tool found.** | [V]/[U] |
| Atomos (Ninja V, Shogun, Inferno, Assassin) | **Built-in recovery**: on restart or playback the unit offers "recover now / later"; all but the last few seconds come back. Fails only if the drive is damaged. | [V] Atomos support |
| ProRes RAW | FixMyVideos: only partial. | [?] |
| Warning | Tools that write 32-bit `stco` instead of `co64` silently corrupt files >4 GB (FixMyVideos). Real, and directly relevant to our validator. | [V] spec fact |

## 4. Action cams, drones, phones, other cinema

| Vendor | Interrupted-file behaviour | In-device repair | Free tools | Mark |
|---|---|---|---|---|
| **GoPro** | MP4 without full index. | **Yes** — "SOS"/bandage/"REPAIRING FILE" icon; press any button (Session: automatic). Card must be in the camera. | untrunc, DiskTuna MP4 Repair (untrunc GUI) | [V] gopro.com help |
| **DJI drones** | MP4 without index; last 2–3 s lost in RAM after crash. | Power-cycle with card in → auto-restore (drone manuals) | **djifix** (outputs raw `.h264`; older models), untrunc, untrunc-GUI. SFWare: djifix fails on Pocket 3 / Mini 4 Pro era files [?]. `.LRF` proxy = low-res fallback. | [V]/[U] |
| **DJI Osmo / Action / Pocket 3** | Same. | Not documented by DJI; vendors say re-insert + power on [?]. | as above | [?] |
| **Insta360** | `.insv` without index; GPS/gyro may be lost. | Some cameras + app/Studio offer repair. | **Insta360 File Repair** (official, free, needs reference for INSV; restores FlowState) | [V]/[?] |
| **iPhone / Android** | Rare in stock Camera (OS finalises); common in 3rd-party camera apps, screen recorders, crashes. iPhone keeps HEVC parameter sets only in `moov` → reference needed. | No | untrunc, nasrtech.dev (browser), Clever Online | [?] |
| **Nikon Z8/Z9, Fujifilm** | Temp-named file left on card [U]; no documented in-camera repair. | No [?] | untrunc | [U] |
| **RED (R3D)** | Header/last frames cut. | **REDUNDEAD** (RED's free recovery tool) + REDCINE-X trim. | REDUNDEAD | [V] docs.red.com |
| **ARRI (MXF)** | Unfinalised MXF. | **ARRI Reference Tool CLI**: `art-cmd copy --input x.mxf --output y.mxf` finalises it. | ART CLI | [V] ARRI manual |
| **Dashcams, OBS, FPV DVRs** | MP4 without index — very high volume (not a "camera" but same failure). OBS now offers crash-safe formats [?]. | No | many tiny untrunc wrappers on GitHub (2026: dashcam-video-repair, FPV-Video-Repair, MaxVideoRepair, cinesalve, PsyChonek/video-repair) | [V] repos |

**Pattern:** RED, ARRI, Atomos, GoPro, Panasonic (MOV), Sony pro camcorders and Insta360 all ship a
free first-party fix. Users miss them because they offload first or google the error. A general
tool's **first job is to say "put the card back in the camera and do X"** when that applies — and
to say it before the user copies files off (Sony pro Restore Media stops working after offload).

## 5. The general-purpose market

### Open source

| Tool | Licence | What it does | Status / quality | Mark |
|---|---|---|---|---|
| **untrunc (anthwlock)** | GPL-2.0 | Reference-based `moov` rebuild; scans `mdat` with codec-specific frame detectors; `-s` step mode; Canon `repi` parser (Nov 2026). | ★3.6k, active. The de-facto engine under most free and some paid tools. Fails on mixed/metadata tracks, PCM, new HEVC profiles. CLI + basic GUI. Issue mentions: Sony 20, DJI 19, GoPro 12, OBS 9, Canon 6. | [V] |
| untrunc (ponchio) | GPL-2.0 | Original. | ★2.0k, inactive since 2024. | [V] |
| recover_mp4 (slydiman) | freeware, closed | Reference profile + raw stream dump (`.h264` + audio) then ffmpeg mux; had Sony/Canon/GoPro templates. | Discontinued; author's engine now runs Restore.Media/fix.video. | [V] |
| mp4fixer (bookkojot) | open | H.264/AAC only. | Old. | [V] |
| djifix | open (C) | DJI-specific stream dump. | Older DJI models. | [U] |
| repair-rsv (macmixing) + forks | GPL-3 | Sony RSV Tauri app on patched untrunc. | Small, Mac-first. | [V] |
| jasonnovak/mp4-reference-repair | MIT | Orchestrator: remux → Sony XAVC → untrunc. | ★1, new. Closest to our idea, tiny. | [V] |
| ffmpeg | LGPL/GPL | Remux (`-c copy`), `-err_detect ignore_err`, `-fflags +genpts`, raw-stream mux. | **Cannot** read MP4 with no `moov`. Fixes damaged-but-present indexes, timestamp issues, raw elementary streams. | [V] |
| VLC | GPL | "Always fix" for AVI index only. | Not an MP4 repair. | [V] |
| MP4Box (GPAC) | LGPL-2.1 | Remux, rebuild from raw streams, fragment/defragment. | Useful mux backend. | [V] |
| Bento4 | GPL-2 / commercial | MP4 parse/dump/mux. | Good for inspection (`mp4dump`). Licence unusual. | [V] |
| PhotoRec / foremost / scalpel | GPL | Signature carving. | PhotoRec handles contiguous and in-order fragmented MP4/MOV (NIST 2015) but **not disordered fragments**; loses names. | [V] NIST |
| REDUNDEAD, ARRI ART CLI, Insta360 File Repair | free, closed | First-party vendor tools. | Good for their own formats. | [V] |

### Closed / paid

| Tool | Price | Free tier | Method | Dark patterns / evidence | Mark |
|---|---|---|---|---|---|
| **Wondershare Repairit** | ~$60–120; monthly/yearly **auto-renew** | Preview only | Quick repair + "Advanced" (reference) | BBB F (company), Trustpilot clusters of surprise renewals and refused refunds; "previews suggested success" then incomplete files; upsell from $70 to $120 Pro Kit. Mostly positive overall ratings. Best real-world Canon evidence. | [V] policy / [U] |
| **Stellar Repair for Video** | $49.99/$59.99/$69.99 per year | Preview only (10 MB–20%, sources differ); online: 50 MB, 10% preview | Remux + reference "Advanced" | Subscription-only per some reviewers. Failed on R6 DAT. | [V]/[U] |
| **EaseUS Fixo** | $39.95/mo; $49.95/yr; $99.95 lifetime | Preview only; online 3/day, ~100–200 MB | Similar | EaseUS pages blanket SEO for every camera. | [V] |
| **Grau Video Repair Tool** | €29 (5 repairs) / €99 unlimited, hardware-bound | Demo repairs half the file | Reference-based index rebuild + raw stream | Honest demo model; Mac build Intel-only. Widely recommended on forums. | [V] |
| Remo / Yodot | $40–100 | Preview only | Reference-based | Same parent (Remo). Thin. | [V]/[?] |
| Kernel Video Repair | $29–39 | 5 videos, no save | Remux-level | | [?] |
| DataNumen | "see website" | demo | | No data found. | [?] |
| **Digital Video Repair** (Rising Research) | free | Full | Index rebuild for AVI/MP4/MOV, mostly old codecs (Xvid/DivX/MPEG-4) | **Installer bundles RelevantKnowledge** (market-research tracker); ZIP download avoids it. | [V] |
| DiskTuna MP4 Repair | free | Full | untrunc build for Windows | Honest; also paid manual service ($50–100+). | [V] |
| **Clever Online Video Repair** | free (account) | Full download | Upload + reference; truncation | Free "during development" — may change; **uploads your footage**. | [V]/[?] |
| **Restore.Media** | from $4.99/file (size-based) | Low-res preview | recover_mp4 engine, server-side, auto + manual | Upload; price unknown until after repair. | [V] |
| **fix.video** | ~$6–12/file, packs $59–99 | Preview | Same author lineage | Upload; one Trustpilot "took €20, didn't repair". | [U] |
| **FixMyVideos** | €6 (<2 GB) → €59 (50–150 GB) | Preview; no cure no pay | Reference-free claims | Upload; transparent pricing; heavy SEO "honest guide" pages per camera. | [V] |
| **MP4repair.org** | $15–79 base + $1–8/min | — | Manual service | Expensive; transparent. | [V] |
| **Aero Quartet Treasured** | from $49, half < $100, pro formats more; 10–20% exploration fee | Watermarked preview | Engineer-built "Repair Kit" app | Strongest pro-format expertise (BRAW, ARRIRAW, XF-AVC, GH5). | [V] |
| rsv.repair | per file (price shown in-app) | First seconds | Local, browser | Went free → paid. | [V] |
| FixMP4.com | $2.99 or $9/mo | ? | Browser-local | | [?] |
| SnapLab, 4DDiG, Video Doctor (Mac App Store) | varies | Preview only | | Video Doctor: preview free, export paid. | [?] |

**The dominant model is "free scan → watermarked or partial preview → pay to save".** That is not
inherently a scam — it is honest when the preview is real (Grau, Treasured, FixMyVideos). The
abuse is (a) subscriptions sold for one-time jobs with auto-renew, (b) previews that overstate the
result, (c) adware bundling, (d) uploading private footage to unknown servers. A free local tool
removes all four.

## 6. Gap analysis

Ranked by estimated volume (proxy: untrunc issue counts, number of vendor SEO pages per camera,
forum threads, and how many tiny GitHub wrappers appeared in 2026). Volume ranks are judgement [?].

| # | Failure class | Volume | Free trustworthy tool today? | Gap |
|---|---|---|---|---|
| 1 | Consumer MP4 with no `moov` (dashcam, phone apps, OBS, screen recorders, action cams) — H.264/HEVC + AAC | Very high | untrunc (CLI, needs reference); many 0-star GUIs | **UX gap**: no trusted, signed, polished app. Reference-free for common profiles is feasible. |
| 2 | Sony `.RSV` (XAVC S/HS MP4 with `rtmd`, LPCM) | High (Sony top of untrunc issues; ≥5 dedicated sites) | repair-rsv (GPL, Mac, needs ref); rsv.repair now paid | **Gap**: free, reference-free, Windows+Mac, signed. |
| 3 | DJI drone/Osmo MP4 after crash/battery | High | djifix (old models), untrunc | Gap for current models (Mini 4 Pro, Avata 2, Pocket 3, Action 4/5). |
| 4 | GoPro unindexed MP4 when SOS fails or card already offloaded | High | untrunc | GPMF telemetry track trips generic tools [?]. |
| 5 | Canon `.DAT` (all modern bodies) | Medium | **DATfix** | Done. |
| 6 | Panasonic `.MDT` in MP4 mode, and GH5/S5 (no in-camera repair) | Medium | untrunc | Gap. |
| 7 | Card-level loss: 0-byte / missing file, deleted, formatted, fragmented (exFAT) | Medium-high, highest value per case | PhotoRec (no fragment reassembly, no index rebuild) | **Big gap**: carve + reassemble + rebuild index in one step. DATfix already carves card images. |
| 8 | BRAW, ProRes MOV (BM, Atomos when unit recovery fails) | Low-medium, high stakes | none | Gap; ProRes is intra-only and easy to frame-scan; BRAW frames are self-delimited. |
| 9 | MXF (Sony XAVC/X-OCN pro, Canon XF-AVC, Panasonic P2) without footer | Low-medium, pro | ARRI only (own MXF) | Gap; MXF index rebuild is well-specified (SMPTE 377). |
| 10 | Insta360 dual-stream `.insv` | Low-medium | Insta360 File Repair (official) | Covered by vendor. |
| 11 | Files that are fine but won't play (10-bit 4:2:2 HEVC, VFR, odd audio) | High (misdiagnosed as "corrupt") | none tells the user | **Diagnosis gap** — cheap, high-trust win. |
| 12 | Mid-file corruption (bad sectors, bit flips) | Medium | ffmpeg `-err_detect ignore_err` | Partial; mostly unrecoverable bytes. |
| — | RED, ARRI, Atomos | — | First-party free tools | Point users to them. |

## 7. Product sketch

### (a) Detection — read-only, < 2 s, on a copy or the original opened read-only

1. **File system context**: extension (`.RSV`, `.MDT`, `.DAT`, `.braw`, `.insv`, `.LRF`), size, sibling files (`.XML`, `.THM`, `.LRF`, `.CPI`, `M4ROOT/`, `PRIVATE/`, `DCIM/100GOPRO`), 0-byte, card image vs file.
2. **Container scan** (top-level box/KLV walk): MP4/MOV boxes (`ftyp` brand, `moov` present/absent/truncated, `mdat` size 0 or wrong, `free` placeholders, `co64` vs `stco`, fragmented `moof`), MXF partition packs (header/body/footer, index table segments), RIFF/AVI `idx1`, MPEG-TS sync bytes, R3D, BRAW.
3. **Vendor signatures**: `ftyp` brand (`XAVC`, `CAEP`, `XFVC`, `qt  `), `uuid` boxes (Canon `repi`, Sony `USMT`/`PROF`), `udta` maker tags, GPMF (GoPro), DJI `djmd`/`dbgi`, Sony `rtmd` prefix, Insta360 trailer.
4. **Bitstream probe** on the first MBs of payload: H.264/HEVC Annex-B vs length-prefixed NALs, length-field size, SPS/PPS/VPS presence in-band (if yes → reference-free possible), ProRes `icpf` frame headers, LPCM vs AAC ADTS vs raw AAC, chunk interleave pattern.
5. **In-file recovery index**: Canon `repi`, Sony salvage data, any vendor equivalent.
6. **Healthy-file check**: if the file is valid, decode test + hardware-decoder check → "Your file is fine; your computer can't decode 10-bit 4:2:2 HEVC. Use X / transcode a copy."
7. **Output: a diagnosis** — vendor, model guess, mode, what is missing, which strategies apply, whether a reference would help, and **in-camera advice** if relevant.

### (b) Strategy battery — least destructive first; every strategy writes a new file

| Order | Strategy | Applies when | Loss |
|---|---|---|---|
| 0 | **Advise in-camera fix** (Sony Restore Media before offload, Panasonic Video Repair, GoPro SOS, DJI power-cycle, Atomos recover, RED REDUNDEAD, ARRI ART) | Vendor + state match | None; user acts |
| 1 | **Remux** (`-c copy`, faststart, regenerate timestamps) | `moov` present but damaged/misplaced; MKV/TS | None |
| 2 | **In-file vendor index rebuild** (Canon `repi`, Sony salvage) | Vendor index found | None to last indexed frame |
| 3 | **Built-in profile rebuild** (no reference): known codec config (SPS/PPS/VPS from in-band or bundled profile per camera+mode), audio format, track layout; walk `mdat` deterministically by interleave pattern | Known vendor/mode | Usually none |
| 4 | **Reference graft**: copy `stsd`/track layout from a user's healthy clip, then frame-walk | Unknown profile, reference supplied | Usually none |
| 5 | **Heuristic frame scan** (untrunc-style, codec-aware NAL/ProRes frame detection; drop unknown chunks) | Interleave unknown | Possible dropped metadata/audio |
| 6 | **Stream dump** → raw `.h264/.hevc` + `.wav/.aac`, then mux with generated timestamps | A/V can't be paired | Sync risk, metadata lost |
| 7 | **Carve from card image** (signature + file-system remnants + vendor patterns, fragment reassembly by bitstream continuity), then 2–6 | 0-byte/missing/deleted/fragmented | Varies |
| 8 | **Salvage decodable ranges** (cut out undecodable GOPs) | Mid-file corruption | Explicit gaps |

Re-encoding is never a repair strategy; only an optional "make an editable copy" export.

### (c) Scoring — pick the best candidate automatically, show the runner-up

Hard gates (fail → candidate rejected): output opens; `co64` used when > 4 GB; no zero-length tracks.
Then score:

- **Recovered duration / estimated total** (estimate from `mdat` bytes ÷ bitrate) — primary.
- **Frame count == index sample count** (`ffprobe -count_frames`).
- **Decode errors** over a full `ffmpeg -v error -f null -` pass (or sampled for huge files, full pass in background) — weight heavily.
- **A/V duration delta** ≤ 1 frame; audio sample count consistent with rate.
- **Keyframe cadence** matches the mode's GOP.
- **Visual sanity**: thumbnails at 10/50/90/last; flag grey/green frames by simple luma/chroma stats.
- Tie-breaks: fewer dropped bytes, metadata tracks kept.

### (d) UX

- One window, one drop zone: file, folder, whole card, or disk image. Never writes to the source; refuses to output onto the source card.
- **Diagnosis card first** in plain words: "This is a Sony FX3 recording that was cut off. All of the video is still in the file. We can rebuild it — no reference clip needed."
- In-camera hint with a "Do this before you copy anything off" warning where it matters.
- Live log: "Trying remux… no index. Trying Sony profile (XAVC HS 4K 23.98 4:2:2)… 18,432 frames found." Collapsible.
- Result: **side-by-side compare** of candidates (thumbnails, duration, frame count, errors), best preselected.
- **Honest loss report**: "Recovered 14:02 of about 14:05. The last 2.8 s were never written to the card. GPS metadata was not recovered." Plus verifiable numbers.
- Ask for a reference clip **only** when the profile is unknown, with exact instructions ("same camera, same resolution, frame rate, codec").
- "Send a sample" button that shares only headers/the first N MB with consent — builds the profile library.
- No account, no upload, no preview-then-pay, no watermark. Say so on the first screen.

### (e) Architecture

- **Rust core** (existing DATfix crates): container parsers (MP4/MOV box walker, MXF KLV, AVI RIFF, TS), bitstream scanners (H.264/HEVC NAL, ProRes, AAC/LPCM), writers (MP4/MOV with `co64`, MXF OP1a later), scorer.
- **Plugin per vendor/format** behind a trait: `detect(&Probe) -> Confidence`, `in_camera_advice()`, `strategies() -> Vec<Strategy>`, `profiles()`. Profiles as data (TOML/JSON) so contributors add camera modes without Rust.
- **ffmpeg** used only for validation (decode check, frame count, thumbnails) and optional remux/export. Options:
  - **Separate `ffmpeg`/`ffprobe` executable, LGPL build, called as a subprocess** (current DATfix approach if applicable). MIT app + separate LGPL program = aggregation; obligations: ship licence, offer the exact ffmpeg source/configure flags, don't claim it's MIT. Cleanest.
  - Dynamic-link `libav*` (LGPL): allowed with MIT; must allow replacing the libs; heavier build matrix.
  - Static link: LGPL relink obligation (provide object files) — avoid.
  - **Never ship a GPL build** (`--enable-gpl`, x264/x265): makes the distributed bundle GPL. Repair needs **no encoders**; FFmpeg's native H.264/HEVC/ProRes/AAC decoders are LGPL.
  - Patents: shipping HEVC/AAC decoders has theoretical patent exposure that licences don't settle; widely ignored by OSS apps, but note it [?, not legal advice].
- **Pure-Rust path**: everything except full decode validation is pure Rust today (parsing, scanning, muxing). For decode validation without ffmpeg: OS decoders (VideoToolbox on macOS, Media Foundation on Windows) or `dav1d`/`openh264`-style crates — partial. Keep ffmpeg as the validator; make core repair work without it.
- **untrunc**: GPL-2 — don't link or bundle in an MIT app; reimplement ideas (its heuristics are well-known). Interop by detecting a user-installed untrunc is possible but not worth it.
- CLI + Tauri app sharing the core; `--json` diagnosis output for scripting and bug reports.

### (f) What to add after Canon (demand × feasibility)

| Rank | Target | Why | Feasibility |
|---|---|---|---|
| 1 | **Generic H.264/HEVC + AAC MP4/MOV, no `moov`** (phones, dashcams, OBS, action cams) with reference graft + in-band SPS detection | Largest volume; core of all others | High — DATfix already writes MP4 |
| 2 | **Sony `.RSV` / XAVC S/HS MP4** (reference-free profiles, `rtmd`, LPCM) | High volume; free option just went paid; pro users | High — fixed interleave, profiles from healthy clips |
| 3 | **DJI + GoPro** (GPMF/`djmd` tracks, LRF fallback) | High volume | Medium-high |
| 4 | **"Not corrupt" diagnosis** (decoder support, VFR, codec) | Cheap; builds trust; cuts misdiagnosis | High |
| 5 | **Card-image carving + fragment reassembly** generalised beyond Canon | Highest value per case; PhotoRec gap | Medium-low (fragmentation is hard) |
| 6 | Panasonic `.MDT` (MP4 mode, GH5/S5) | Medium | High (same as #1) |
| 7 | ProRes MOV (Atomos/BM/Panasonic ProRes) and BRAW | Pros pay $100–200 per file today | Medium (ProRes easy; BRAW frame format less documented) |
| 8 | MXF footer/index rebuild (Sony XAVC/X-OCN, Canon XF-AVC) | Pro niche | Medium (spec is public, writing MXF is work) |

### (g) Naming

"DATfix" names a Canon file extension. It is good SEO for "Canon DAT repair" and bad for anything
else; Sony/GoPro/phone users will not try it. Recommendation: **a general brand with DATfix kept
as the Canon mode and its landing page** (and the existing repo/URL redirecting). Candidates
(not trademark- or domain-checked [?]):

| Name | Note |
|---|---|
| **Reclip** | Short, verb-able ("reclip it"), fits the ClipLogger/ClipSweeper family. |
| **ClipMend** | Explicit; family fit. |
| **Unbroke** | Friendly, idiot-proof tone. |
| **FrameSaver** | Plain meaning; check conflicts. |
| **Moovfix** | Technical (names the `moov` box); good SEO for "moov atom not found", obscure to normal users. |

---

## Sources

Sony
- https://helpguide.sony.net/ilc/2035/v1/en/contents/TP1000243639.html (FX3 Recover Image DB)
- https://helpguide.sony.net/ilc/2040/v1/en/contents/TP1000384598.html (ILCE-1)
- https://forums.tomsguide.com/threads/sony-fs100u-repair-image-database-missing-video-files.444454/latest
- https://www.manualslib.com/manual/1997690/Sony-Pxw-Fx9.html?page=136 (FX9 Restore Media)
- https://creativecow.net/forums/thread/battery-diedrestore-media-message/
- https://www.dvxuser.com/threads/restore-media-error-message-while-recording.342569/
- https://www.xdcam-user.com/2018/03/05/use-the-cameras-media-check-to-help-ensure-you-dont-get-file-problems/
- https://www.dvinfo.net/forum/sony-xdcam-pxw-fs7-fs5/532729-card-errors-message-media-needs-repaired.html
- https://rsv.repair/ · https://rsv.repair/docs/rsv-format/
- https://www.cined.com/rsv-repair-by-ottomatic-introduced-free-recovery-tool-for-sony-rsv-files/
- https://github.com/macmixing/repair-rsv · https://github.com/raiz0tsky/repair-rsv · https://hansley.fr/en/rsv-sos/
- https://footage.repair/ · https://rsvrepair.com/
- https://github.com/anthwlock/untrunc/issues/137
- https://www.jeffgeerling.com/blog/2025/recovering-videos-my-sony-camera-i-stupidly-deleted/
- https://github.com/SK-Hardwired/xavc_rtmd2srt
- https://www.fredmiranda.com/forum/topic/1672296/0
- https://community.adobe.com/bug-reports-728/hevc-4-2-2-10-bit-6-2k-video-not-using-hardware-decoding-works-fine-on-4k-1332219
- https://www.sanbila.com/blog/h265-hevc-lagging-premiere-pro

Panasonic
- https://eww.pavc.panasonic.co.jp/dscoi/DC-GH6/html/DC-GH6_DVQP2441_eng/0165.html
- https://eww.pavc.panasonic.co.jp/dscoi/DC-GH7/html/DC-GH7_DVQP3109_eng/0121.html
- https://eww.pavc.panasonic.co.jp/dscoi/DC-S5M2/html/DC-S5M2_DVQP2839_eng/0160.html
- https://www.manualslib.com/manual/3147821/Panasonic-Lumix-S5iix.html?page=520
- https://aeroquartet.com/treasured/panasonic_lumix_gh5.en.html
- https://fstoppers.com/gear/using-third-party-batteries-can-corrupt-your-video-files-173969
- https://www.dpreview.com/forums/thread/4021660

Blackmagic / Atomos
- https://forum.blackmagicdesign.com/viewtopic.php?f=2&t=146540 · https://forum.blackmagicdesign.com/viewtopic.php?f=2&t=175161 · https://forum.blackmagicdesign.com/viewtopic.php?f=2&t=42658 · https://forum.blackmagicdesign.com/viewtopic.php?f=3&t=63539
- https://aeroquartet.com/treasured/blackmagic-raw.en.html · https://www.brawrecovery.se/en · https://brawrecovery.com/
- https://www.gillware.com/data-recovery-services/recover-unfinalized-video-file/
- https://support.atomos.com/hc/en-us/articles/5457698485263-File-Recovery · https://support.atomos.com/hc/en-us/articles/5109827471247-File-Recovery
- https://fixmyvideos.com/fix/atomos-shogun-prores-corrupted

GoPro / DJI / Insta360 / phones / others
- https://gopro.com/help/articles/solutions_troubleshooting/Camera-Displays-SOS
- https://forum.dji.com/thread-271869-1-1.html · https://www.handyrecovery.com/repair-corrupt-dji-files/ · https://www.sfware.com/blog/repair-corrupt-dji-video-files
- https://4ddig.tenorshare.com/file-tips/insta360-file-repair-full-guide.html · https://repairit.wondershare.com/video-repair/repair-insv-windows-mac.html
- https://www.nasrtech.dev/tools/video-repair/iphone-video-wont-play/
- https://onlinemanual.nikonimglib.com/z9/en/16_troubleshooting_03.html · https://www.dpreview.com/forums/thread/4637606
- https://docs.red.com/955-0076/REDUNDEADOperationGuide/Content/1_Overview/REDUNDEADOverview.htm
- https://www.arri.com/resource/blob/402608/20ac446709713dfa4271aa47f633c439/arri-user-manual-art-cmd-v1-0-0-data.pdf
- https://aeroquartet.com/treasured/arriraw.en.html

Market
- https://github.com/anthwlock/untrunc · https://github.com/ponchio/untrunc · https://github.com/jasonnovak/mp4-reference-repair · https://github.com/bookkojot/mp4fixer · https://github.com/gpac/gpac · https://github.com/axiomatic-systems/Bento4
- https://www.trustpilot.com/review/repairit.wondershare.com · https://repairit.wondershare.com/support/refund-and-renewal-service-faqs.html · https://www.softorbits.net/how-to/wondershare-repairit-review.html · https://bestguide.com/review/wondershare-recoverit/
- https://www.hitpaw.com/video-tips/stellar-repair-for-video.html · https://repair.stellarinfo.com/
- https://www.easeus.com/shopping/new/fixo-one-month.html · https://repair.easeus.com/video-repair/fixo-video-repair-review.html
- https://www.cleverfiles.com/howto/repair-corrupted-video.html
- https://sourceforge.net/software/product/Grau-Video-Repair/ · https://www.stellarinfo.com/article/best-video-repair-software.php
- https://www.disktuna.com/mp4-video-repair/ · https://www.disktuna.com/disktuna-projects-page/ · https://www.disktuna.com/mp4-mov-fragmented-video-recovery/
- https://www.risingresearch.com/en/dvr/ · https://www.trishtech.com/2024/09/digital-video-repair-free-software-for-fixing-corrupt-video/
- https://restore.media/ · https://forum.videohelp.com/threads/390330-Reparation-of-corrupted-(-)-MOV-video-files
- https://medium.com/@best-software/best-ways-to-repair-corrupt-unreadable-mp4-mov-video-files-online-in-2024-eb80d8c1405a · https://www.trustpilot.com/review/fix.video
- https://fixmyvideos.com/ · https://mp4repair.org/pwt6/faq2.html · https://fixmp4.com/
- https://aeroquartet.com/treasured/pricing.en.html
- https://www.remosoftware.com/faq/can-i-use-remo-video-repair-software-free-of-charge · https://sourceforge.net/software/product/Yodot-MOV-Repair/ · https://sourceforge.net/software/product/Kernel-Video-Repair/
- https://apps.apple.com/us/app/video-doctor/id1492318174?mt=12
- https://www.dhs.gov/sites/default/files/publications/508_Test%20Report_NIST_PhotoRec%20v7.0-WIP_0_August%202015_Final_0.pdf

Licensing
- https://ffmpeg.org/legal.html · https://medialooks.com/lgpl · https://www.learninternetgrow.com/ffmpeg-license-ladder/ · https://news.ycombinator.com/item?id=38161593

Demand proxy (GitHub issue search on anthwlock/untrunc, 2026-11-26): Sony 20, DJI 19, GoPro 12, OBS 9, Canon 6, iPhone 5, dashcam 4, Samsung 3, ProRes 3, Panasonic 2, Nikon 2, Insta360 1, Blackmagic 1, Fuji 1.
