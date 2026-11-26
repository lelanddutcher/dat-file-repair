# DATfix user guide

DATfix repairs the `.DAT` file that a Canon camera leaves when a recording is cut off. It writes a
new `.MP4` next to it. Your `.DAT` is only read. DATfix never changes or deletes it.

This guide covers:

1. [Before you start](#before-you-start)
2. [Repair with the app](#repair-with-the-app)
3. [Repair with the command line](#repair-with-the-command-line)
4. [Recover a 0-byte or missing `.DAT`](#recover-a-0-byte-or-missing-dat)
5. [Troubleshooting](#troubleshooting)

## Before you start

- **Copy the card to your computer.** Copy the whole card folder, not only the `.DAT`. The healthy
  clips next to it can help the repair. Reading from the internal drive is faster and safer than
  reading from the card.
- **Keep the card as it is** until your video is back. Do not record on it or format it.
- **Check free space.** The repaired video is about as big as the `.DAT`.

Where to find the `.DAT`: in the folder where the camera saves its clips, usually under `DCIM`
(for example `DCIM/100CANON/`), or under `XFVC` for XF-HEVC S recordings. The name looks like the
camera's normal clips, for example `LAD12345.DAT` or `A_0012C345H250101_120000AB.DAT`.

## Repair with the app

### 1. Add the file

Open DATfix. Drop the `.DAT` file into the window, or click the drop zone and choose it.

![The DATfix window with an empty drop zone](img/01-drop-zone.png)

You can add several `.DAT` files at once. To repair everything on a card, click
**Choose the card folder** under the drop zone. DATfix finds every `.DAT` on it.

DATfix reads the file. For a large file this takes up to a minute. It then shows:

- **Camera**: the model, or a family guess such as "Canon EOS camera with PCM audio".
- **Contents**: video, sound, number of audio channels, timecode.
- **About … of video can be recovered**: the length DATfix found.
- **Cut off at the end**: "Yes" means the camera stopped in the middle of a group. DATfix leaves
  out that unfinished last moment, which is under one second.

![The file summary after DATfix read a .DAT](img/02-file-summary.png)

### 2. Video settings

DATfix needs the camera's decoder settings to rebuild the video. By default it finds them for you.
It looks for a matching healthy clip in the `.DAT`'s folder (or the whole card, if the `.DAT` is on
a card). If no clip matches, it uses its built-in settings for your camera mode.

The step shows which source it chose, for example
"Using DATfix's built-in settings for your camera. No clip needed." or
"Using the clip you chose: LAD12344.MP4."

![The Video settings step showing built-in settings](img/03-video-settings.png)

To choose yourself:

- **Choose a clip…**: pick one healthy `.MP4` from the same camera, recorded with the same
  resolution, frame rate and sound settings. A clip shot just before or after is best.
- **Search a folder…**: DATfix checks every clip in that folder and its subfolders, and lists
  why each one does or does not fit.
- **Try without a reference** or **Use built-in settings instead**: use the built-in settings
  only.
- **Choose automatically**: go back to the default.

A clip you choose always wins over the built-in settings. If only its video fits, DATfix uses its
video settings and rebuilds the audio and timecode tracks. It tells you when it does this.

### 3. Save and repair

By default the repaired video goes next to the `.DAT`, named `<name>_repaired.mp4`. DATfix never
replaces an existing file; it adds `(2)`, `(3)` and so on. To pick another place, choose a folder
or a file name in step 3.

Press **Repair**. The progress bar shows the three steps: checking the settings, reading the video
frames and writing the repaired video. You can stop at any time. A stopped repair leaves no partial
file behind.

![A repair in progress](img/04-progress.png)

When it finishes, DATfix shows "Your video is repaired." and the recovered length. Click
**Show in Finder** (macOS) or **Show in Explorer** (Windows) to find the file.

![The finished screen](img/05-done.png)

### Leftover files from a stopped repair

If DATfix or your computer stopped during a repair, a hidden unfinished file may be left in the save
folder. DATfix finds these and offers to delete them ("Found 1 unfinished file …"). They cannot be
played. Deleting them is safe.

## Repair with the command line

The command is `datfix`. Each command prints plain words first and the technical detail below.
Add `--json` to any command for newline-delimited JSON events.

### See what is in a `.DAT`

```sh
datfix inspect LAD12345.DAT
```

It lists the streams, the sample counts, an estimate of the duration and whether the file is cut
off.

### Repair

```sh
datfix repair LAD12345.DAT
```

This writes `LAD12345_repaired.MP4` in the current folder. Without a reference option, DATfix
first tries the clips in the `.DAT`'s own folder, then its built-in settings.

| Option | What it does |
|---|---|
| `--reference <clip.MP4>` | Use this healthy clip from the same camera. It always wins over the built-in settings. |
| `--reference-dir <folder>` | Search this folder, 3 levels deep, for a matching camera-original clip. Re-encoded files are skipped. |
| `--builtin` | Use the built-in settings only. |
| `-o, --output <file>` | Where to write the MP4. |
| `--force` | Replace the output file if it exists. |

Examples:

```sh
datfix repair LAD12345.DAT --reference LAD12344.MP4 -o ~/Movies/ceremony.MP4
datfix repair A_0012C345.DAT --reference-dir /Volumes/EOS_DIGITAL
```

### List the built-in settings

```sh
datfix profiles
```

Each profile holds decoder settings only: no video, thumbnails or location data.
`datfix profiles harvest <folder>` builds a new set from your own healthy camera clips.

### Remove leftovers from a stopped repair

```sh
datfix clean            # list hidden unfinished files in the current folder
datfix clean ~/Movies --yes   # delete them
```

## Recover a 0-byte or missing `.DAT`

A `.DAT` of 0 bytes holds no video, and some cards show no `.DAT` at all after a cut. The
recording is often still on the card, outside any file. `datfix scan` finds it by its content.
This feature is in the command line only for now.

### 1. Stop using the card

Do not record on it, format it, or let the computer or camera "repair" it. Each write can overwrite
your video.

### 2. Make a raw image of the card

The scan only reads, but an image protects you if the card fails during a second read.

**macOS, Disk Utility:**

1. Insert the card. In Disk Utility, choose **View > Show All Devices**.
2. Select the card **device** (the top line, not the volume under it).
3. Choose **File > New Image > Image from "…"**.
4. Set **Format** to **DVD/CD master** (or **read/write**). Compressed images do not work.
5. Save it to a drive with enough free space (as large as the card).

**macOS, Terminal:**

```sh
diskutil list                                 # find the card, for example disk4
diskutil unmountDisk disk4
sudo dd if=/dev/rdisk4 of=card.img bs=4m status=progress
```

Check the disk number twice. `dd` reads from `if=` and writes to `of=`.

**Windows:** use a raw imaging tool, for example HDD Raw Copy, or `dd` in Git Bash. The card is a
device such as `\\.\PhysicalDrive2`. Save a raw image (`.img`), not a compressed one.

**Linux:** `sudo dd if=/dev/sdX of=card.img bs=4M status=progress conv=noerror,sync`. For a
failing card, use `ddrescue`.

### 3. Scan the image

```sh
datfix scan card.img                          # report only
datfix scan card.img -o recovered             # save recovered_<offset>.DAT files
datfix scan card.img -o recovered --repair    # also repair each one to an MP4
```

`--repair` uses finished clips from the same card as references, then the built-in settings.

| Option | What it does |
|---|---|
| `--clips` | Also save every finished Canon clip found, including deleted ones. |
| `--fragments` | Also save pieces of recordings whose start is gone. |
| `--no-stitch` | Do not join recordings that the card split into pieces. |

`<offset>` in a file name is the byte position on the card where the recording starts.

The scan finds a recording only while its data has not been overwritten. It cannot find recordings
from the EOS R or M50 Mark II. It reads raw images and exFAT/FAT32 cards; convert a compressed
`.dmg` first with `hdiutil convert -format UDTO in.dmg -o out`. The repaired file's date is the scan
time; the timecode still has the time of day. Full details:
[card scanning](../format/card-carving.md).

Another free option is [PhotoRec](https://www.cgsecurity.org). Open the `.DAT` or `.MP4` files it
finds in DATfix.

## Troubleshooting

Each error in the app has a title, what happened, and **What to do**. **Copy details** copies the
technical text for a bug report. The command line prints the same words.

### This .DAT file is empty

The `.DAT` holds no recording data. The camera created the file but never saved the video into it.
Follow [Recover a 0-byte or missing `.DAT`](#recover-a-0-byte-or-missing-dat).

### This file can't be repaired

The file has no Canon recovery index. Check that it is the `.DAT` from a Canon camera card, and
that it copied completely.

### This isn't a Canon camera .DAT

The file is from a Video CD, a DVD recorder or another device. These `.dat` files are not broken.
Open them in VLC, or rename an MPEG one to `.mpg`.

### This camera's .DAT can't be repaired yet

The file comes from a Canon camera that writes no recovery index, such as the EOS R or M50 Mark II.
Keep the `.DAT` unchanged and
[share a sample](https://github.com/lelanddutcher/dat-file-repair/issues/new/choose) so a later
version can support it.

### DATfix needs a healthy clip from this camera

DATfix has no built-in settings for this mode. Choose one clip that plays normally, from the same
camera, with the same resolution, frame rate and sound settings. Then please send a camera report so
the next version includes this mode.

### DATfix needs a healthy clip to be sure

More than one set of built-in settings fits, and the video alone cannot tell which is right.
Choose a healthy clip from the same camera and mode.

### The clip doesn't match this .DAT

The clip was recorded with different settings. The screen lists each difference. Choose another
clip from the same camera and mode, or repair without a clip.

### No matching clip found

None of the clips in the folder was recorded with the same settings. The screen lists why each one
was skipped. Choose a clip from the same camera and mode, or use the built-in settings.

### The healthy clip can't be read

The clip does not open as a video. It may be damaged or still copying. Choose a clip that plays.

### A file with that name already exists / That would overwrite the original

DATfix never replaces files and never writes to the `.DAT`. Choose another name or folder.

### The save folder is gone

The save folder was removed or the drive was disconnected. Choose another place to save.

### Not enough free space / The disk is full

The repaired video needs about as much space as the `.DAT`. Free up space or save to another
drive. If the disk filled during the write, DATfix removed the partial file.

### No permission

DATfix cannot read or write in that place. A locked card or a read-only folder causes this. Choose
another folder. On a Mac, check **System Settings > Privacy & Security > Files and Folders**.

### File not found

The card or drive was removed. Reconnect it and add the file again.

### The file ended early

The file is shorter than expected. It may still be copying. Wait for the copy to finish.

### The card or disk reported a read error

Part of the file could not be read. Copy the `.DAT` to your computer's internal drive first (a
card recovery tool can help), then repair the copy.

### A disk error stopped the job

The operating system reported a read or write error. Check that the card or drive is connected and
has free space, then try again.

### This is a folder

You dropped a folder where DATfix expected a `.DAT`. Drop the `.DAT` itself, or use
**Choose the card folder**.

### No .DAT files were found there

The folder you chose has no `.DAT` files. A `.DAT` is what a Canon camera leaves when a recording is
cut off. If the card shows none, see [Recover a 0-byte or missing `.DAT`](#recover-a-0-byte-or-missing-dat).

### Stopped

You stopped the repair. No partial file was left. Press **Repair** to start again.

### Something went wrong inside DATfix

This is a bug. Click **Copy details** and paste the text into a
[bug report](https://github.com/lelanddutcher/dat-file-repair/issues/new/choose).

### The repaired file shows the wrong date

DATfix dates the file from, in order: the date in the Canon file name, the card's directory entry
(card recovery only), the timecode, and the file's own time. The report's "date source" line says
which one it used. A camera whose clock or timecode was set wrong gives a wrong date; fix it in
your editor. DATfix never copies the reference clip's thumbnail, serial numbers or location unless
you pass `--keep-reference-metadata`.
