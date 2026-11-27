# AGENTS.md — DATfix

Rules for anyone, human or coding agent, who changes this repo. `PLAN.md` holds the goal,
milestones and status. `CONTRIBUTING.md` holds the contributor workflow.

Maintainers: private operational notes live outside the repo.

## Data safety (hard rules)

- Treat the original recordings as read only. Copy a file to a scratch folder before you work
  on it, and keep test copies read only (mode `0444`).
- Media never enters git. No `.DAT`, `.MP4`, `.CRM`, `.hevc`, `.h264` or extracted `.jpg` frames.
- Repair output is always a new file. The tool must never open its input for writing.
- Do not write paths, names or metadata from your own recordings into docs or tests. Use neutral
  ids (`R5II-01`) and environment variables (`DATFIX_CORPUS`, `DATFIX_SAMPLES`).

## Test media

Tests that need real recordings are skipped unless their environment variables are set. See the
comment at the top of each file in `crates/datfix-core/tests/` and `app/src-tauri/tests/`.

- `DATFIX_CORPUS`: a folder of `.DAT` files, each next to healthy clips from the same camera.
- `DATFIX_OUT`: where corpus outputs go (default: a temporary folder).
- `DATFIX_SAMPLES`: a folder with small samples, for example `r5m2_64MB.DAT`, used in examples.
- `DATFIX_DAMAGE_DAT`, `DATFIX_REF`, `DATFIX_APP_*`: single-file tests; see each test's header.

## Verifying a repair

A repair counts only when all of these pass:

1. `ffprobe -count_frames` frame count equals the `repi` video sample count.
2. `ffmpeg -v error -i out.mp4 -f null -` prints nothing.
3. Audio duration equals video duration within one frame.
4. A frame from the middle and the last GOP is extracted and looked at.

## Dev loop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd app && npm ci && npm run build      # front-end typecheck + bundle
cd app && cargo tauri dev              # run the desktop app
```

The app has dev hooks for scripted runs and screenshots (`DATFIX_DEV_AUTORUN`,
`DATFIX_DEV_THEME`); see `docs/dev/rust-learnings.md`.

## Working style

- One change per branch, named by intent (`feat/repi-walker`, `fix/…`, `docs/…`).
- If you change behaviour that `docs/format/` describes, update the doc in the same change.
- Stuck 3 times on the same error: stop and report.

## Layout

- `crates/datfix-core` — the repair engine (Rust library).
- `crates/datfix` — the `datfix` CLI.
- `app/` — the Tauri desktop app.
- `tools/` — throwaway research scripts (Python, run with `python3 -I`).
- `scripts/` — release helpers.
- `docs/` — format notes, research, test results, decisions.
