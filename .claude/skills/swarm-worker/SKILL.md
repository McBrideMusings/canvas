---
name: swarm-worker
description: "How a swarm worker builds and tests Canvas beside other workers: heavy commands, the shared target folder, per-checkout throwaway daemon and dev app, and what never to touch (the live daemon, launchd, /Applications). Read before the first build or test in a swarm worktree."
---

# Canvas swarm worker

Every worker builds into the main checkout's `target/` (`CARGO_TARGET_DIR`, set by swarm).
Both Cargo workspaces, the root one and `app/src-tauri`, land there. A binary in it is whatever
the last worker built, so never run `target/debug/canvas` or `target/debug/app` by path: run
the copies `bash scripts/verify-app.sh start` makes, as below.

## Heavy commands

Run each through `"$HOME/.claude/skills/swarm/swarm" heavy -- <command>`:

- `cargo build …`, `cargo test …`, `cargo clippy …`, `cargo check …` (either workspace)
- `cargo test --workspace` and `cargo check --manifest-path app/src-tauri/Cargo.toml` (what `admin test` runs)
- `bash scripts/verify-app.sh start` (builds both workspaces, then copies the binaries out)
- `cargo tauri build …`
- `bash scripts/readme-shots.sh`, `bash scripts/clip-demo.sh`, `bash scripts/seed.sh`:
  each writes a fixed `/tmp/canvas-*` folder, so the heavy lock also keeps two from colliding

## The cheap check

`cargo fmt --all -- --check` needs no lock. For "does it compile", run
`swarm heavy -- cargo check -p <crate>` (`canvas-core`, `canvasd`, `canvas`) once per round of
edits, and the full `swarm heavy -- cargo test --workspace` plus `swarm heavy -- cargo check --manifest-path app/src-tauri/Cargo.toml` before the review step.

## Per-worker resources

- **Throwaway daemon and dev app.** `bash scripts/verify-app.sh start` puts each checkout in its own
  `/tmp/cv-<hash of the checkout path>`: socket, data dir, logs, debug folder and `bin/` copies
  of `canvas` and `app`. Then `eval "$(bash scripts/verify-app.sh env)"` sets `CANVAS_DATA_DIR`,
  `CANVAS_SOCKET` and puts that `bin/` first on `PATH`, so `canvas …` is this worktree's build.
  `shot` and `eval` find the window by this checkout's app pid. Where `verify-project` writes
  `target/debug/canvas` or `app/src-tauri/target/debug/app`, use `canvas` from that `PATH`,
  or `"$(bash scripts/verify-app.sh dir)/bin/canvas"` and `…/bin/app`. Run `bash scripts/verify-app.sh stop`
  before the pass ends.
- **Shared, so leave alone:** the dev app's localStorage (`~/Library/WebKit/app`) is one store
  for every checkout's dev app. Set the theme through `canvas theme light|dark` in your own
  verify env, never assume it.
- **`canvas record --window app`** records the frontmost matching window, which may be another
  worker's dev app. Hold the heavy lock while recording:
  `swarm heavy -- canvas record --window app <out> --seconds N`.

## Never in a worker

- Any `admin` task: in a worktree it runs the main checkout's tree. `admin dev`, `service`,
  `deploy` and `kill` also touch the live
  socket in `~/Library/Application Support/canvas`, the launchd agent, `~/.local/bin/canvas` or
  `/Applications/Canvas.app`, which belong to the person's running Canvas.
- `cargo clean`: it deletes every worker's build.
