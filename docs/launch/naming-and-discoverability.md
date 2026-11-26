# Naming and GitHub discoverability (webmaster research, 2026-11-26)

Status: recommendation. Leland decides the final name before the repo goes public.

## Recommendation

- Product **DATfix**, CLI `datfix`, repo `datfix`. Free on crates.io and GitHub (checked 2026-11-26).
- Keep "Canon" out of the product name (trademark risk). Use it in the description, topics and README
  ("for Canon cameras") plus the line: *Not affiliated with or endorsed by Canon Inc. Canon and EOS are
  trademarks of Canon Inc.*
- Alternatives: datmend, datrescue, datrevive, dat2mp4 (good alias command; attracts VCD-DAT users).

## Search intent

- Main query: "canon dat file repair" / "how to open .dat file canon". Only paid-tool blogs rank. No free local tool ranks.
- People search by model ("canon r6 dat file") and by cause ("battery died during recording canon", "Err70").
- "convert dat to mp4" is a different audience (VCD/DVR). Mention once, don't target.
- Most common unrecoverable case: 0-byte DAT. README must say so and point to card recovery (PhotoRec).

## GitHub

Description (≤350 chars):
> Free, open-source repair for Canon .DAT video files — the file a Canon EOS R5, R6, R3 or Cinema EOS camera leaves when recording is cut off (battery died, card error, Err70). Rebuilds a playable MP4 without re-encoding, using a good clip from the same camera. Desktop app and CLI for macOS and Windows. MIT.

Topics: canon, canon-eos, canon-r5, canon-r6, dat-file, video-repair, mp4-repair, video-recovery,
corrupted-video, file-repair, data-recovery, mp4, h264, hevc, untrunc, rust, tauri, cli, desktop-app, open-source

README order: summary → two intro paragraphs → downloads → "Is it free?" → supported cameras
(Verified/Expected/Not supported; Verified only after AGENTS.md checks pass) → usage (app, CLI) → why a
reference clip → 0-byte DAT → comparison → how it works (repi) → FAQ in search phrasing → disclaimer → contributing (send samples).

Release assets: `DATfix_<v>_macos_universal.dmg`, `DATfix_<v>_windows_x64-setup.exe`,
`datfix-cli-<v>-{aarch64,x86_64}-apple-darwin.tar.gz`, `datfix-cli-<v>-x86_64-pc-windows-msvc.zip`, `SHA256SUMS.txt`.

Social preview: "Canon .DAT file? Get your video back. Free, open-source repair for EOS R5 · R6 · R3 · Cinema EOS — macOS & Windows".

## Landing page

GitHub Pages one-pager in the repo (title tag, FAQ + SoftwareApplication schema, OS-aware download
button), linked from lelanddutcher.com projects. Launch with ≥2 Verified cameras.

## Where to announce (not posted)

untrunc issues #170, #215, #303 (check #84 is Canon first); Canon Community threads 439135, 395783,
95506, 189727; DPReview R6 II thread 4774045; EOSHD topic 46746; Creative COW Canon DAT thread;
gist coltenkrauter/e77d229f7b4afc7fa347cf0099d69d33; r/canon, r/videography, r/WeddingVideography,
r/editors, r/datarecovery; Show HN; AlternativeTo (alternative to Repairit, Grau).
Re-check competitor prices on vendor sites on release day.
