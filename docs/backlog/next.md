# Engine backlog (orchestrator-owned)

Queued while `feat/hardening` and `feat/card-carve` own `crates/`.

1. ~~**Reference metadata leak (privacy, high).** The Canon `uuid` box (THMB/CNTH thumbnail, CMT1–4 EXIF:
   body serial, lens, firmware, dates, GPS) is copied verbatim from the reference into the output.
   Example: LAD10430 repaired with LAD19893 carries LAD19893's thumbnail, lens and 2021-12-11 EXIF dates.
   Fix: by default drop the thumbnail and GPS and rewrite or remove EXIF dates/serial; keep only what
   players and NLEs need. Add `--keep-reference-metadata` for users who want it. Test with exiftool.~~
   Done in `feat/engine-v3`: the Canon `uuid`, `meta` and all `udta` children except maker and
   model are left out; `--keep-reference-metadata` restores them.
2. ~~**Creation date** fallback still uses DAT mtime when the filename has no date; prefer tmcd + per-frame
   records; document limits.~~ Done: timecode time of day + file-time date when they agree
   (records hold no date); `date_source` in the report.
3. ~~**Error text** for layout failures must not tell the user to pick another reference when the engine is at fault.~~ Done: `Error::LayoutNotSupported`
   (needs the issue form `.github/ISSUE_TEMPLATE/unreadable-recording.yml`, not yet written).
4. ~~**AAC synthesis** for hybrid references (no corpus DAT needs it yet).~~ Done: bank
   template for the same body, else AAC-LC 48 kHz from the first frame's channel element.
5. ~~**Carved recordings get the scan time as "recorded" time.** Pass the directory-entry timestamp or
   original `.DAT` name from `carve` into repair.~~ Done: `carve::hint_for` and
   `RepairOptions::hint`; 0-byte entries whose first cluster is 0 still cannot be matched.
6. ~~**Rotation** for vertical recordings.~~ Done: `--rotate` / `RepairOptions::rotation`.

# App backlog

1. **"Recover from card" flow** for `datfix scan` (see the UI suggestions in `docs/format/card-carving.md`
   and the card-carve agent's report): offer it on the `EmptyDat` error and as its own action; image the
   card first; live progress and results; "Repair all" using the card's own clips as references.

# Release backlog

1. Release workflow must also build the CLI archives named in the README
   (`datfix-cli-<v>-{aarch64,x86_64}-apple-darwin.tar.gz`, `datfix-cli-<v>-x86_64-pc-windows-msvc.zip`)
   and `SHA256SUMS.txt`, and attach everything to a GitHub Release on tag.
2. After engine v3 and the app card-recovery flow merge: update README privacy note and the guide's
   "card recovery is CLI-only" line.
