# Backlog (orchestrator-owned)

Done items are struck through and say where they landed. Open items are listed first in each
section.

## Open

1. **Windows build on a real machine.** The installer and CLI archive build in CI but have not
   been run on Windows. The app's card flow lists no cards on Windows (`list_cards` is macOS only);
   card images still work.
2. **Signing and notarization** (macOS Developer ID, Windows code-signing certificate). Needs an
   account and money; see `docs/dev/rust-learnings.md`, "Signing and notarization".
3. **Admin imaging path not tested by a person.** "Make an image of the card" on a real card reader
   asks for the administrator password; only the no-password path (an attached disk image) was
   tested.
4. **Premiere Pro compatibility** of repaired files is not tested.
5. **Card zone sign on macOS-written exFAT.** A test card written by macOS stored its exFAT time
   zone as UTC+05:00 while the Mac was at UTC-05:00 (the stored local time was shifted the same
   way, so the UTC instant is right). A real Canon card has not been checked; if a camera writes
   the same pattern, a time from a Canon file name would be off by twice the zone.

## Engine

1. ~~**Reference metadata leak (privacy, high).**~~ Done in `feat/engine-v3`: the Canon `uuid`,
   `meta` and all `udta` children except maker and model are left out;
   `--keep-reference-metadata` restores them.
2. ~~**Creation date** fallback.~~ Done: timecode time of day + file-time date when they agree;
   `date_source` in the report.
3. ~~**Error text** for layout failures.~~ Done: `Error::LayoutNotSupported` and the issue form
   `.github/ISSUE_TEMPLATE/unreadable-recording.yml`.
4. ~~**AAC synthesis** for hybrid references.~~ Done.
5. ~~**Carved recordings get the scan time as "recorded" time.**~~ Done: `carve::hint_for` and
   `RepairOptions::hint`. 0-byte entries with no first cluster are now matched by the start time
   in their name (or their creation time) against the timecode (`FileMatch::StartTime`), so the
   CLI and the app name and date them too (`feat/final-integration`).
6. ~~**Rotation** for vertical recordings.~~ Done: `--rotate` / `RepairOptions::rotation`.
7. ~~**EmptyDat message** pointed to PhotoRec first.~~ Done: it now points to "Recover from a
   memory card" / `datfix scan`, with PhotoRec as the fallback.
8. Skipped: `carve::scan_source(ImageSource)`. The app scans through a test throttle wrapper, so
   the helper would save one line.

## App

1. ~~**"Recover from card" flow**~~ Done in `feat/app-card-recovery`.
2. ~~**Engine v3 options in the app.**~~ Done in `feat/final-integration`: per-file "Advanced"
   (Rotate, keep reference metadata), recording time and its source plus withheld metadata in the
   result's Details, "Report this recording" for `layout_not_supported`, card copies repaired
   with `hint_for`.

## Release

1. ~~**Release workflow** with CLI archives and `SHA256SUMS.txt`.~~ Done in
   `chore/release-workflow`.
2. ~~README privacy note and the guide's "card recovery is CLI-only" line.~~ Done in
   `feat/final-integration`, with real screenshots in `docs/user/img/`.
