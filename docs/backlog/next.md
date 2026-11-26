# Engine backlog (orchestrator-owned)

Queued while `feat/hardening` and `feat/card-carve` own `crates/`.

1. **Reference metadata leak (privacy, high).** The Canon `uuid` box (THMB/CNTH thumbnail, CMT1–4 EXIF:
   body serial, lens, firmware, dates, GPS) is copied verbatim from the reference into the output.
   Example: LAD10430 repaired with LAD19893 carries LAD19893's thumbnail, lens and 2021-12-11 EXIF dates.
   Fix: by default drop the thumbnail and GPS and rewrite or remove EXIF dates/serial; keep only what
   players and NLEs need. Add `--keep-reference-metadata` for users who want it. Test with exiftool.
2. **Creation date** fallback still uses DAT mtime when the filename has no date; prefer tmcd + per-frame
   records; document limits.
3. **Error text** for layout failures must not tell the user to pick another reference when the engine is at fault.
4. **AAC synthesis** for hybrid references (no corpus DAT needs it yet).
