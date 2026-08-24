# spark

*A lightweight data-driven game engine in Rust — 2D and 3D, one architecture, one binary.*

```
   engine core      4,417 lines
   codegen macros     171 lines
   editor + runner  1,379 lines
   ─────────────────────────────
   total             6,967 lines   (target ≤ 10,000 · ceiling 15,000, CI-enforced)
```

[![CI](https://github.com/salom600/spark/actions/workflows/ci.yml/badge.svg)](https://github.com/salom600/spark/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](LICENSE-MIT)

spark is a complete game engine in under seven thousand lines of Rust: a unified
2D/3D renderer on wgpu, an integrated graphical editor, rapier physics, rodio
audio, human-readable scenes, and a rule-based behavior system that replaces
scripting with **data** — the same engine binary is the editor *and* the game
runner.

```rust
// A game component in spark costs one declaration:
#[derive(ComponentDef, Clone, Default, Serialize, Deserialize)]
struct Health { value: f32, max: f32 }
registry.register::<Health>();   // done: saved, loaded, inspected, cloned, edited
```

```ron
// And gameplay costs zero Rust — rules are data, hot-reloadable:
( on: KeyPressed("Space"),
  when: [ Var(scope: Entity, name: "grounded", op: Eq, value: 1) ],
  run: [ SetVelY(( y: 10.5 )),
         PlaySound( sound: "assets/sfx/jump.wav", volume: 0.7 ) ] )
```

---

## Quick start

**Prerequisites:** Rust 1.85+ (stable). On Linux: `libasound2-dev libudev-dev`
(`sudo apt install libasound2-dev libudev-dev`). Windows needs nothing extra.

```bash
git clone https://github.com/salom600/spark.git
cd spark

cargo run -p spark_editor            # open the editor
cargo run -p spark_editor -- --game demos/ember_run      # play the 2D demo
cargo run -p spark_editor -- --game demos/playground     # play the 3D demo
cargo test --workspace               # 21 tests, GPU-free
```

### First project in the editor

1. **File → New Project…** — pick a name and 2D/3D. A project (camera, light,
   `project.ron`, `scenes/main.scene`) is scaffolded for you.
2. **Scene → Add 2D Sprite / Add Cube / Add Point Light** — entities appear in
   the Hierarchy.
3. Select an entity, edit components in the **Inspector** (every widget you
   see is generated from one derive macro).
4. Drop `.png` / `.glb` / `.wav` files into the project's `assets/` folder —
   they appear in the **Asset browser** instantly (hot reload).
5. Add a **Rules** component and wire events → conditions → actions.
6. **F5** to play in-editor; F5 again to restore the exact pre-play scene.
7. **Project → Export Game…** — packages the runnable game folder.

### Controls (editor viewport)

| Input | Action |
|---|---|
| Left-drag | pan |
| Right-drag | orbit (3D) |
| Wheel | zoom |
| F5 | play / stop |
| Ctrl+Z / Ctrl+Y | undo / redo (via Edit menu) |

## Bundled demos (all assets procedurally generated)

| Demo | What it shows |
|---|---|
| **Ember Run** (`demos/ember_run`) | 2D platformer: run/jump physics, coin collection, hazards with respawn, camera follow, win state — *zero Rust* |
| **Physics Playground** (`demos/playground`) | 3D sandbox: spawn boxes/balls (Space/B), toggle gravity (G/H), clear (C), shadows + point lights |

## Why spark is small

Every subsystem is built to *multiply* rather than *add*:

- **`#[derive(ComponentDef)]`** — one declaration generates the inspector UI,
  nested-edit support, and registration metadata. Every component you add
  makes the engine comparatively smaller.
- **Generic registry** — save/load/clone/duplicate/inspect for any component
  through ~40 type-erased lines instead of N × boilerplate.
- **Rules instead of a scripting language** — an event→condition→action
  vocabulary replaces ~1,500 lines of interpreter with ~650 lines of
  evaluator, keeps scenes hot-reloadable, and stays sandboxed by design.
- **One frame graph** — sprites and PBR meshes flow through the same
  materials, cameras, and asset pipeline; 2D is an orthographic camera, not a
  second engine.
- **The editor is the engine** — play mode snapshots the scene through the
  same serializer that saves files; there is no second runtime to maintain.

Trade-offs accepted along the way are documented in
[DECISIONS.md](DECISIONS.md) — nothing is silently cut.

## Repository layout

```
crates/spark          engine core (renderer, ECS, physics, rules, audio, assets)
crates/spark_macros   #[derive(ComponentDef)] codegen
crates/spark_editor   the editor + game runner binary (`spark`)
demos/                Ember Run (2D) and Physics Playground (3D)
templates/blank       new-project template
tools/                count_loc.py (CI budget guard), gen_assets.py
docs/                 architecture deep-dive
.github/workflows     Windows + Ubuntu CI
```

## Documentation

- [BUILDING.md](BUILDING.md) — build, run, package for Windows/Linux
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — module-by-module deep dive
- [DECISIONS.md](DECISIONS.md) — every engineering decision and its trade-offs
- [ROADMAP.md](ROADMAP.md) — what's next
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to help

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) — your
choice. The same terms as most of the Rust ecosystem it builds on.
