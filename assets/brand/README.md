# DATfix brand assets

Icon direction "Splice", bandage variant, by the creative desk (2026-11-26). The film strip is
mended with a bandage: the footage was cut off and DATfix makes it whole again. No Canon marks.

- `../icon.svg`: macOS icon master (squircle). `glyph.svg`: transparent glyph, used for the Windows `.ico`.
- App icons in `app/src-tauri/icons/` come from the desk's PNGs; `icon.icns` uses the hand-tuned
  16 and 32 px masters (built with `iconutil`), so do not regenerate it with `cargo tauri icon` alone.
- `readme_banner_1280x320_{light,dark}.png`: README header.
- `social_preview_1280x640.png`: upload in GitHub Settings > General > Social preview (no API).
- `og_1200x630.png`: for a future GitHub Pages site.
- Design sources and rebuild steps are kept outside the repo. Fonts: Inter, IBM Plex Mono (OFL). No AI imagery.
