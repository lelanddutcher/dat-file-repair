# Existing Canon .DAT repair tools (survey, 2026-11-26)

## Summary

- No free tool repairs R5 Mark II (`XFVC`) or R3 `.DAT` files today.
- The closest is **anthwlock/untrunc**. On 2026-11-01 (commit `81fc1c9`) it added a parser for Canon's
  embedded `repi` index. That path runs only for brand `CAEP` with HEVC + AAC + timecode and no other
  tracks. An R6 II user confirmed a full recovery with it (issue #303).
- R5 II reports against untrunc fail ("part-length -1094995529", issue #84 comment, Nov 2025). Our
  analysis explains why: R5 II files use brand `XFVC`, 4-channel 24-bit LPCM and a `tdmt`/`cndm`
  metadata track, so the `repi` path is skipped and the generic scanner misreads `repi` headers as frames.

## Open source

| Tool | Canon result |
|---|---|
| untrunc (ponchio) | Finds no frames on R5-era files. Inactive. |
| untrunc (anthwlock) | R6 II fixed (Nov 2026). R5: partial (#170, open). R5 C: crash (#215). R5 II: fails (#84). |
| recover_mp4 (slydiman) | Old H.264 Canon templates only. Discontinued; engine now behind Restore.Media. |
| mp4fixer, recover-mp4, fix-corrupted-videos, remoover | H.264/AAC only. |
| "dat2mp4" repos | VCD `.dat`, not Canon. |
| jeanpoole gist | One C70 RAW Light case; splices a CRM shell onto the DAT. No index rebuild. |
| ffmpeg | Cannot read MP4 data without `moov`. |

## Commercial software

| Tool | Price | Notes |
|---|---|---|
| Grau Video Repair Tool | €29 / 5 repairs, €99 unlimited | Claims Canon DAT + CRM. No R5/R5 II/R3 success found. |
| Wondershare Repairit | ~$60–120 | Best user evidence (R6, R6 II). No R5 II/R3 reports. |
| Stellar Repair for Video | $50–70/yr | Failed on R6 DAT. |
| EaseUS Fixo | ? | One C100 II success. |
| Clever Online Video Repair | dynamic | Claims Canon DAT; 4:2:2 10-bit not mentioned. |

## Paid services

| Service | Price |
|---|---|
| Aero Quartet Treasured | quote; from $49, cinema formats more; can exceed $1,000 |
| MP4repair.org | $79 + $8/min (first 10 min), then $4/min |
| Restore.Media | from $4.99/file automatic; no DAT/4:2:2 listed |
| RichardRecovery | unpublished; claims R5 HEVC DAT, C70 |
| FixMyVideos | unpublished; reference-free R5 MP4; no CRM/MXF |

## Other Canon facts gathered

- RP (horshack, EOSHD): camera reserves the header area in front of the `.DAT`, writes `ftyp`+`moov` on stop.
- Cinema bodies (C70/C200): interrupted RAW leaves `.CRM` + `.DAT` + `.DMV`. Cinema bodies have an
  in-camera [Recover]; EOS stills bodies do not document one.
- XF-AVC MXF (C70/C400/C80/R5 C video mode): missing footer/index; no tool rebuilds it.
- 0-byte or missing `.DAT`: data is on the card but unlisted; needs a card image and carving.

## Sources

- untrunc: github.com/anthwlock/untrunc (issues #84, #170, #215, #278, #303; commits 81fc1c9, 9e03db0, b4bbc4e)
- EOSHD horshack thread pages 4–5 (topic 46746)
- aeroquartet.com/treasured/pricing.en.html · mp4repair.org/pwt6/faq2.html · main.grauonline.de/video-repair-tool/
- fixmyvideos.com · richardrecovery.com · repair.cleverfiles.com
- Canon R5 II manual: cam.start.canon/en/C017/manual/
- lclevy/canon_cr3 · AdrianEddy/canon-raw · gist jeanpoole/54f1e923034cf973bd473bd846246e72
