# AGENTS.md — DAT file repair

Rules for any agent working in this repo. `PLAN.md` holds the goal, milestones and current status.

## Data safety (hard rules)

- Never write, rename or delete anything on the NAS (`/Volumes/zpool`, `/Volumes/media`, `root@192.168.0.197`).
  Read only. Copy a file to the workspace before you touch it.
- Workspace for media: `/Volumes/15T/dat-file-repair/` (`samples/`, `refs/`, `out/`). Copies in
  `samples/` are mode `0444`.
- Media never enters git. No `.DAT`, `.MP4`, `.CRM`, `.hevc`, `.jpg` frames in commits.
- Repair output is always a new file. The tool must never open its input for writing.

## Finding files

- Fast search: `ssh root@192.168.0.197 'find /mnt/storage0/zpool ...'` (server-side; SMB `find` takes hours).
- SMB share `zpool` = `/mnt/storage0/zpool`; `media` = `/mnt/storageold/media`.
- Corpus index: `/Volumes/15T/dat-file-repair/scan/` (TSV of every `.DAT` > 20 MB).

## Orchestration

- The main session is the orchestrator. It owns `PLAN.md` and assigns work.
- At most 3–5 concurrent agents. Each gets owned files and a pass/fail check.
- One file, one owner. Worktrees are named by intent (`feat/repi-walker`) and removed after merge.
- Stuck 3 times on the same error: stop and report.

## Verifying a repair

A repair counts only when all of these pass:

1. `ffprobe -count_frames` frame count equals the `repi` video sample count.
2. `ffmpeg -v error -i out.mp4 -f null -` prints nothing.
3. Audio duration equals video duration within one frame.
4. A frame from the middle and the last GOP is extracted and looked at.

## Layout

- `tools/` — throwaway research scripts (Python, run with `python3 -I`).
- `crates/` — the Rust core and CLI (from M1).
- `app/` — the Tauri desktop app (from M4).
- `docs/` — research, format notes, decisions.
