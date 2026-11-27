<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/readme_banner_1280x320_dark.png">
  <img src="assets/brand/readme_banner_1280x320_light.png" width="640" alt="DATfix: get your video back from a cut-off Canon .DAT recording. Free and open source, for Mac and Windows.">
</picture>

# DATfix: repair a Canon `.DAT` video file

**A Canon camera leaves a `.DAT` file instead of an `.MP4` when a recording is cut off: the
battery died, the card was removed, or the camera stopped with an error such as Err 70.** DATfix is
a free, open-source app for Mac and Windows, and a command-line tool, that turns that `.DAT` into an
MP4 you can play and edit, with no re-encoding and nothing uploaded.

DATfix is verified on the Canon EOS R5 Mark II, EOS R5 and EOS R3. It can also find a recording
on the memory card when the `.DAT` is 0 bytes or missing.

## What a Canon `.DAT` file is

A Canon camera records into a temporary `.DAT` file. When you stop recording, the camera writes the
file's index and renames it to `.MP4`. If the battery dies, the card fails or is removed, or the
camera shows an error, the camera never writes that index. Your video is still in the `.DAT`, but no
player or editor can open it. Renaming the file to `.MP4` does not help, because the index is still
missing.

DATfix rebuilds the missing index from the recovery data that Canon writes inside the `.DAT`. It
copies the video and audio unchanged, with no re-encoding and no quality loss. It runs on your
computer, uploads nothing, and never changes the original file.

## Download

[Download the latest release](https://github.com/lelanddutcher/dat-file-repair/releases/latest).

| You have | Download |
|---|---|
| Mac (Apple silicon or Intel) | `DATfix_<version>_macos_universal.dmg` |
| Windows 10 or 11 (64-bit) | `DATfix_<version>_windows_x64-setup.exe` |
| Command line, Mac (Apple silicon) | `datfix-cli-<version>-aarch64-apple-darwin.tar.gz` |
| Command line, Mac (Intel) | `datfix-cli-<version>-x86_64-apple-darwin.tar.gz` |
| Command line, Windows | `datfix-cli-<version>-x86_64-pc-windows-msvc.zip` |

`SHA256SUMS.txt` lists a checksum for each file.

- **macOS:** the app is signed with a Developer ID and notarized by Apple, so it opens normally.
- **Windows:** the installer is not code-signed yet. SmartScreen may warn about an unknown
  publisher. Choose **More info**, then **Run anyway**. It is tested on Windows 11.

## Is it free?

Yes. DATfix is free for any use, with no trial, watermark or file-size limit. The code is under
the [MIT license](LICENSE).

## Supported cameras

**Verified** means real `.DAT` files from that camera and mode were repaired and every repair
passed every check: the frame count matches the index, ffmpeg decodes the whole file with no
errors, audio and video have the same length, and frames from the middle and end look right. The
last full run was on 26 November 2026: 36 real `.DAT` files, 273 GB, each repaired twice (once with
a clip from the same folder when one matched, once with the built-in settings only), 72 of 72
passed. [Test results](docs/testing/corpus-results.md).

| Camera | Recording mode | Status | Verified with |
|---|---|---|---|
| EOS R5 Mark II | XF-HEVC S, 4K DCI 119.88p, 4:2:2 10-bit, 4 channels 24-bit PCM | Verified | 15 real `.DAT` files, 0.07 to 37.8 GB. Also opened in QuickTime and DaVinci Resolve 21.2. |
| EOS R5 | HEVC 4K DCI 59.94p All-I, 4:2:2 10-bit, stereo PCM | Verified | 10 real `.DAT` files, 0.07 to 1.2 GB |
| EOS R3 | HEVC 4K DCI 29.97p All-I, 4:2:2 10-bit, no audio | Verified | 10 real `.DAT` files, 0.13 to 31.9 GB. Also opened in QuickTime and DaVinci Resolve 21.2. |
| EOS R3 | HEVC 4K DCI 59.94p All-I, 4:2:2 10-bit, stereo PCM | Verified | 1 real `.DAT` file, 0.77 GB |
| EOS R5 Mark II | Other XF-HEVC S frame rates (for example 59.94p) | Expected | Not yet tested on a real `.DAT` |
| EOS R5, R6, R6 Mark II | Modes with AAC audio | Expected | Not yet tested on a real `.DAT` |
| EOS R6 Mark II | HEVC with PCM audio | Expected | Not yet tested on a real `.DAT` |
| Cinema EOS C70, C80 | HEVC with AAC or PCM audio | Expected | Not yet tested on a real `.DAT` |
| EOS R, EOS M50 Mark II | Any | Not supported | The camera writes no recovery index |
| Any | Cinema RAW Light (`.CRM`) | Not supported | Different file structure |

**Expected** means the camera writes the same recovery data. We know the layout from healthy clips,
but have not yet repaired a real `.DAT` from it. If you have one, please
[send a sample](https://github.com/lelanddutcher/dat-file-repair/issues/new/choose).

**Not supported:** the EOS R and M50 Mark II write no recovery index into the `.DAT`, so DATfix
cannot find the frames. Cinema RAW Light uses a different file structure. DATfix tells you when it
sees these files and does not guess.

## Usage

**App.** Open DATfix and drop your `.DAT` file (or the whole card folder) into the window. DATfix
reads the file and picks the video settings. Choose where to save, then press **Repair**. Each file
has an **Advanced** section: **Rotate** turns the picture for clips filmed on the side, and **Keep
the reference clip's camera metadata** is off by default (see [Privacy](#privacy)). The
[user guide](docs/user/guide.md) walks through every step.

<img src="docs/user/img/05-done.png" width="480" alt="DATfix after a repair: Recovered 1.1 s, with Show in Finder and Play buttons">

**Command line.**

```sh
datfix inspect A001.DAT                         # what is in the file, and how much can be recovered
datfix repair A001.DAT                          # writes A001_repaired.MP4 in the current folder
datfix repair A001.DAT --reference A002.MP4 -o fixed.MP4
datfix repair A001.DAT --rotate 90              # play the picture turned 90 degrees clockwise
datfix scan card.img -o recovered --repair      # find lost recordings on a card image
```

Run `datfix --help` or `datfix <command> --help` for every option. Add `--json` to any command
for machine-readable output.

## Why a reference clip can help

A Canon `.DAT` holds the frames, but not the decoder settings (the codec's parameter sets). The
camera writes those only into the finished MP4. DATfix needs them to make the video playable.

DATfix has built-in settings for the common modes, taken from real camera files, so you often need
nothing else. If it has none for your mode, give it a **reference clip**: one healthy `.MP4` from
the same camera, recorded with the same resolution, frame rate and audio settings. A clip shot just
before or after the broken one is best. Without a choice from you, DATfix first looks for a matching
clip in the `.DAT`'s own folder, then uses its built-in settings.

## My `.DAT` file is 0 bytes (or missing)

A 0-byte `.DAT` holds no video. The camera created the file entry but never updated it. On some
cards the entry is missing completely. The recording is often still on the card, outside any file.

1. Stop using the card. Do not record on it, format it or let the computer "repair" it.
2. In the app, click **Recover from a memory card…** (or **Scan the memory card for this
   recording…** under the "This .DAT file is empty" message). DATfix can make an image of the
   card for you, or scan an image you already have. It lists what it finds; **Repair all** copies
   each recording off the card and repairs it.
3. On the command line, make a raw image of the card (see the
   [user guide](docs/user/guide.md#recover-a-0-byte-or-missing-dat) for macOS and Windows steps)
   and run `datfix scan card.img -o recovered --repair`.

The scan only reads. It also finds deleted clips and joins recordings that the card split into
pieces when it can prove the order. A recovered recording keeps its original file name when the
card's file list still has it, also for a 0-byte entry (matched by its start time). The repaired
file then gets the recording time from that name or from the card's file list, not the time of
the scan. The scan is tested on exFAT and FAT32 card images that hold real `.DAT` recordings, with
one copy deleted and one cut to 0 bytes: both came back byte for byte.
[How card scanning works](docs/format/card-carving.md).

## How DATfix compares

Prices and claims below were checked on each vendor's own page on 27 November 2026. They may have
changed since. Sale prices are marked.

| Tool | Cost | Canon `.DAT` notes |
|---|---|---|
| DATfix | Free, MIT license | Reads Canon's recovery index for every track. Runs on your computer. Verified modes are listed above. |
| [untrunc](https://github.com/anthwlock/untrunc) (anthwlock fork) | Free, GPL-2.0 license | Pioneered this kind of repair. Since 1 November 2026 it reads Canon's recovery index for HEVC files with AAC audio and timecode; an R6 Mark II user reported a full recovery ([#303](https://github.com/anthwlock/untrunc/issues/303)). Reports for the R5 ([#170](https://github.com/anthwlock/untrunc/issues/170)), R5 C ([#215](https://github.com/anthwlock/untrunc/issues/215)) and R5 Mark II ([#84](https://github.com/anthwlock/untrunc/issues/84)) are still open. |
| [Grau Video Repair Tool](https://main.grauonline.de/video-repair-tool/) | 29 EUR for 5 repairs or 99 EUR unlimited, one computer, plus VAT in the EU. No refunds. | Lists Canon `.DAT` files. The free demo repairs half of each file. |
| [Wondershare Repairit](https://repairit.wondershare.com/buy/store.html) | Windows: US$39.99 a month, $69.99 a year or $99.99 perpetual (sale prices; list $69.99, $99.99, $129.99) | Says its Professional Mode handles camera `DAT` files left by a power failure. |
| [Stellar Repair for Video](https://www.stellarinfo.com/disk-recovery/video-repair.php) | US$49.99 Standard; $59.99 Professional and $69.99 Premium (sale prices; list $109.99, $149.99) | General video repair. Names Canon among supported camera brands. |
| [StreamSalvage](https://www.streamsalvage.com/guides/canon-dat-file-recovery) | US$29 one time, Windows only, free preview | Publishes a Canon `.DAT` guide. Rebuilds the file from a reference clip. |
| [MP4repair.org](https://mp4repair.org/pwt6/faq2.html) (online service) | From $15. Professional formats: $79 plus $8 a minute for the first 10 minutes, then $4 a minute | You upload the file. |
| [Treasured](https://www.aeroquartet.com/treasured/pricing.en.html) by Aero Quartet (service) | A quote after they see a sample. Their chart says half of jobs cost under $100. | A technician works on your file. |

In our search (November 2026) we found no public reports of the paid tools repairing R5 Mark II or
R3 `.DAT` files. That does not mean they cannot. If a tool or service works for you, use it.

## How it works

While it records, a Canon camera writes the video, audio and timecode in groups. Before each group,
it writes a small header that starts with the letters `repi`. The header lists the track type, the
number of samples and the exact size of each one. Canon wrote these headers so a camera can recover
its own files. Together they form a complete index of the recording.

DATfix walks the `repi` headers from the start of the `.DAT`, records where every frame and audio
sample starts, and stops at the first group the camera did not finish. It then writes a new MP4:
the file-type box, a rebuilt header (`moov`) with the sample tables, and the `.DAT` bytes copied
unchanged. The decoder settings come from the built-in profiles or your reference clip.

Details: [Canon `.DAT` format](docs/format/canon-dat-format.md) ·
[card scanning](docs/format/card-carving.md).

## Privacy

DATfix runs on your computer. It never uploads your files and works without an internet connection.

A repaired file does not carry another clip's identity. DATfix copies only the settings a player
needs from the reference clip. It leaves out that clip's thumbnail, lens, serial numbers, dates and
location, and takes the camera model and recording date from your `.DAT`. To keep the reference
clip's Canon metadata instead, turn on **Keep the reference clip's camera metadata** under
**Advanced** in the app, or use `--keep-reference-metadata`. The result's **Details** list what was
left out.

## FAQ

### What is a `.DAT` file from a Canon camera?

It is a recording the camera did not finish. The camera writes video into a `.DAT` and renames it
to `.MP4` when you press stop. If recording is cut off, the `.DAT` stays on the card with the video
inside but without the index players need.

### How do I open a `.DAT` file from my Canon camera?

You cannot open it directly. Repair it with DATfix, then open the new MP4. Renaming it to `.MP4`
does not work, because the index is still missing.

### My battery died while recording. Can I get the video back?

Usually yes. The `.DAT` holds everything up to the cut. DATfix leaves out only the unfinished last
moment, which is under one second.

### I removed the card while recording. Can I get the video back?

Often yes. Put the card in your computer and look for the `.DAT`. If it has a size, repair it with
DATfix. If it is 0 bytes or missing, use **Recover from a memory card…** to scan the card. Stop
using the card until your video is back.

### My camera showed Err 70 and left a `.DAT`. Can DATfix repair it?

Yes, if the `.DAT` comes from a supported camera. The error stops the camera before it writes the
index, the same as a dead battery.

### My `.DAT` file is 0 bytes. Is the video gone?

Not always. A 0-byte `.DAT` holds no video itself, but the recording is often still on the card.
DATfix can scan the card for it. See [My `.DAT` file is 0 bytes](#my-dat-file-is-0-bytes-or-missing).

### Which Canon cameras does DATfix support?

Verified: EOS R5 Mark II, EOS R5 and EOS R3, in the modes listed in the table. Expected to work,
but not yet tested on a real `.DAT`: EOS R6, R6 Mark II, C70 and C80 in some modes. Not supported:
EOS R and EOS M50 Mark II. See [Supported cameras](#supported-cameras).

### Does DATfix change my original file?

No. It only reads the `.DAT` and always writes a new file.

### Does it re-encode or lower the quality?

No. The video and audio bytes are copied as they are.

### Does it upload my video?

No. DATfix runs on your computer and works without an internet connection.

### Do I need a healthy clip from the same camera?

Often not. DATfix has built-in settings for the verified modes. If it has none for yours, it asks
for one healthy `.MP4` from the same camera and mode.

### Will the file work in Premiere Pro, DaVinci Resolve or Final Cut?

Repaired files open in QuickTime and DaVinci Resolve 21.2 with all tracks and the camera's
timecode. Premiere Pro and Final Cut Pro are not tested yet.

### How is DATfix different from untrunc?

Both are free and rebuild the index from a healthy clip's settings. untrunc reads Canon's recovery
index for HEVC files with AAC audio. DATfix reads it for every track, including PCM audio, which is
what the R5 Mark II, R5 and R3 use in the verified modes. DATfix also has a desktop app, built-in
settings and a card scan for 0-byte files.

### My `.DAT` came from a Video CD or DVR, not a camera.

Those are MPEG files, not broken ones. Open them in VLC. DATfix tells you when it sees one.

### Does DATfix fix broken `.MP4` or `.MXF` files?

No. It repairs Canon `.DAT` files only.

## Build from source

You need stable Rust and Node.js.

```sh
cargo build --release -p datfix                       # CLI: target/release/datfix
cd app && npm ci && cargo tauri build                 # desktop app (needs tauri-cli)
```

## Contributing

The most useful thing you can send is a `.DAT` from a camera or mode that is not Verified, with
one healthy clip from the same camera. Open a
[camera report](https://github.com/lelanddutcher/dat-file-repair/issues/new/choose). Code
contributions are welcome too: see [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems
as described in [SECURITY.md](SECURITY.md).

## License and trademarks

MIT. See [LICENSE](LICENSE).

Not affiliated with or endorsed by Canon Inc. Canon and EOS are trademarks of Canon Inc.
