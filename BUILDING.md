# Building spark

Supported platforms: **Windows 10/11** and **Linux** (Ubuntu 24.04 tested),
x86_64. Both build from the same source with no platform-specific code.

## Prerequisites

| | Requirement |
|---|---|
| All | Rust **1.85+** (stable) via [rustup](https://rustup.rs) |
| Linux | `sudo apt install libasound2-dev libudev-dev` (audio + gamepad) |
| Windows | nothing beyond Rust (Visual Studio C++ build tools, standard for Rust) |

Other distros: install the ALSA and libudev development packages
(`alsa-lib-devel systemd-devel` on Fedora, `alsa-lib libudev-dev` on Arch).

## Build & run

```bash
# from the repository root
cargo build --workspace               # debug build (~2 min cold)
cargo run -p spark_editor            # launch the editor
cargo run -p spark_editor -- --game demos/ember_run    # run the 2D demo
cargo run -p spark_editor -- --game demos/playground   # run the 3D demo

cargo test --workspace               # 21 tests — GPU-free, audio-free
cargo clippy --workspace --all-targets -- -D warnings  # what CI checks
cargo fmt --all -- --check
python3 tools/count_loc.py           # line-count budget report
```

First build downloads and compiles ~450 crates (wgpu, egui, rapier, …);
subsequent builds are incremental. On a 2-core CI runner expect 10–15 minutes
for a cold build, ~30 seconds incremental.

## Graphics requirements

wgpu 27 targets Vulkan (Linux/Windows), DX12 (Windows) and Metal; the engine
also enables the GLES backend as a compatibility fallback for older GPUs and
VMs. Any GPU from the last decade works; software rendering (llvmpipe,
lavapipe) works for the demos but is slow.

## Release build & packaging

```bash
cargo build --release -p spark_editor
```

The binary `target/release/spark` (`.exe` on Windows) is **both** the editor
and the game runner:

| Command | Behavior |
|---|---|
| `spark` | opens the editor |
| `spark --game <project-dir>` | runs the project as a standalone game |

A runnable game folder is just: the `spark` binary + a project directory
(`project.ron`, `scenes/`, `assets/`). The editor's **Project → Export
Game…** produces exactly that layout in `<project>/export/`. To distribute:
zip the export folder — no installation, no registry entries, no runtime
dependencies on Windows; on Linux the ALSA/udev runtime libs are part of every
desktop distro.

### Demo assets

All sprites and sounds in `demos/` are generated — regenerate with:

```bash
python3 tools/gen_assets.py    # requires: pip install pillow
```

## CI

`.github/workflows/ci.yml` runs on every push: Ubuntu 24.04 + Windows latest
matrix — build (all targets), test, clippy with `-D warnings`, rustfmt check,
line-count ceiling (`tools/count_loc.py --fail-over 15000`), then builds and
uploads release artifacts (binary + demos + templates) from both platforms.

## Troubleshooting

- **`alsa-sys`/`libudev-sys` build errors (Linux)** — install the dev packages
  above; they are only needed at build time.
- **No audio device** — fine: spark logs once and runs silent (CI proves it).
- **`AdapterNotFound` / wgpu instance errors** — you're likely on a machine
  with no GPU access at all (plain container). Run tests instead:
  `cargo test --workspace` never touches the GPU.
- **Editor opens to an empty scene** — that's the no-project state; use
  File → New/Open Project.
