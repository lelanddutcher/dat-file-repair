# Contributing to DATfix

Thank you for helping. Two kinds of help matter most.

## 1. Send a sample from your camera

DATfix marks a camera mode **Verified** only after it repairs a real `.DAT` from that mode and the
result passes every check. Most modes are still **Expected**. One sample can change that.

1. Open a [camera report](https://github.com/lelanddutcher/dat-file-repair/issues/new?template=camera-report.yml).
2. Give the camera model, firmware version and recording settings.
3. Paste the output of `datfix inspect --json <file>.DAT`.
4. Say whether you can share the `.DAT` and one healthy `.MP4` from the same camera and mode. A
   short clip is enough. We will agree a private way to send it.

We use samples only to test DATfix. We never commit media to the repository.

## 2. Contribute code

### Layout

- `crates/datfix-core`: the repair engine (Rust library).
- `crates/datfix`: the `datfix` command-line tool.
- `app/`: the desktop app (Tauri: Rust backend, TypeScript front end).
- `docs/format/`: the Canon `.DAT` format and card scanning. Read these before you change the engine.
- `tools/`: research scripts.

### Build and test

You need stable Rust and Node.js.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release -p datfix
cd app && npm ci && cargo tauri build
```

CI runs the first three on macOS, Windows and Linux.

Tests on real recordings are skipped unless you set `DATFIX_CORPUS` to a folder of `.DAT` files
and their reference clips. See the comment at the top of `crates/datfix-core/tests/corpus.rs`.

### Rules

- **Never write to the input.** The engine opens the `.DAT` and the reference read-only, and the
  output is always a new file. Keep it that way.
- **No media in git.** No `.DAT`, `.MP4`, `.CRM`, `.hevc` or extracted frames.
- **No guessing without saying so.** If DATfix cannot be sure, it refuses with a clear message.
- **Two texts per error.** A plain message for people (`Error::user_message`) and a technical one
  (`Display`). The app and the CLI show the same words.
- If you change behaviour that `docs/format/` describes, update the doc in the same pull request.

### Commits and pull requests

- Name the branch by intent: `feat/…`, `fix/…`, `docs/…`, `chore/…`.
- Commit subject says what changed. The first line of the body says why.
- Keep a pull request to one change. Describe how you tested it.

By contributing, you agree that your contribution is licensed under the [MIT license](LICENSE).
