# spark — Engineering Decision Record (Phase 0)

Status: **Accepted** · Date: 2026-08-24 · Applies to: spark 0.1.x

This document records every significant engineering decision made while designing spark,
the alternatives considered, and the measurable trade-offs accepted. It is the authoritative
answer to "why is spark built this way?".

---

## 1. Design pillars

1. **Small, honest core** — under 10,000 non-blank, non-comment lines of engine+editor code
   (hard ceiling 15,000), enforced by CI. Every line must earn its place.
2. **One architecture for 2D and 3D** — sprites and meshes flow through the same frame
   graph, material system, and scene model. 2D is not a bolted-on afterthought.
3. **The editor is the engine** — editor and exported game are the *same binary* with the
   same code path; the editor is an overlay, not a separate application.
4. **Data over code** — behavior that used to demand scripting is expressed as
   event→condition→action rules inside scene files, hot-reloadable, zero interpreter.
5. **Programmers still write Rust** — spark is a library first; games that outgrow rules
   drop to plain Rust systems against the public API. No language lock-in.

---

## 2. Language: Rust (decided)

| Criterion | Rust 1.98 | Zig 0.14/0.15 |
|---|---|---|
| Graphics (wgpu) | first-class, same org | bindings immature/community |
| Physics (rapier) | native | none comparable |
| Audio (rodio/cpal) | native | partial C interop |
| Editor UI (egui) | native | none comparable |
| Memory safety | borrow-checked, no GC | manual, safety is explicit |
| Cross-platform build in CI | `cargo` — one line | build system churn pre-1.0 |
| Macro codegen (LOC reduction) | proc-macros, mature | comptime (strong, but ecosystem gap above dominates) |

**Decision:** Rust. The requirement "use mature external libraries instead of reinventing
the wheel" is disqualifying for Zig in the graphics/physics/UI/audio domain as of 2026 —
choosing Zig would force re-implementing four ecosystems. Zig remains attractive for future
helper tooling; it is not used in 0.1.

---

## 3. Library survey (versions resolved 2026-08-24)

| Concern | Choice | Version | Alternatives rejected (why) |
|---|---|---|---|
| Graphics API | **wgpu** | 27.0 | raw Vulkan/GL (x4 platform backends to maintain); bgfx-rs (weaker Rust story); Vulkan→GL fallback achieved via wgpu's `gles` backend instead of a second renderer |
| Windowing | **winit** | 0.30 | tao (split maintenance); sdl2 (brings its own everything) |
| Editor UI | **egui** + egui-wgpu + egui-winit | 0.33 | iced (retained-mode boilerplate per panel; weak fit for live reflection editing); custom UI (~+2,000 LOC, violates pillar 1) |
| Entity system | **hecs** | 0.10 | bevy_ecs (schedules/queries we don't need, large dep tree); legion (unmaintained); ship ECS by hand (reinventing the wheel) |
| Physics | **rapier2d + rapier3d** | 0.19 | physx (C++); nphysics (superseded by rapier); box2d bindings (2D only) |
| Audio | **rodio** | 0.21 | kira (more features, more code paths); cpal directly (we'd rebuild rodio) |
| Model import | **gltf** | 1.0 | assimp bindings (C++ surface, huge); FBX/OBJ out of scope (glTF is the runtimes standard) |
| Image import | **image** | 0.25 | stb bindings; dxtex (BCn only). PNG/JPEG/HDR covered |
| Gamepad | **gilrs** | 0.11 | ggp (unmaintained) |
| Scene/serialization | **ron + serde** | 0.9 / 1 | JSON (no comments, worse diffs for humans); YAML (footguns, slower) |
| File watching (hot reload) | **notify** | 8.0 | inotify directly (Linux-only, reinvents wheel) |
| Math | **glam** | 0.30 | nalgebra (general but slower-compile, awkward ergonomics); cgmath (unmaintained) |
| GPU data | **bytemuck** | 1.23 | manual transmutes (unsafe, error-prone) |
| Errors | **anyhow** | 1.0 | thiserror in library paths where a typed error adds value; anyhow keeps call sites 1 line |

All external code lives in dependencies — none of it counts against the LOC budget,
consistent with the project constraints.

---

## 4. Architecture

```
                        ┌──────────────────────────────────────────┐
                        │              spark (binary)              │
                        │  ┌─────────────┐      ┌───────────────┐  │
   .ron project ──────► │  │ Editor mode │ /───►│  Game mode    │  │
   assets/ ───────────► │  │ egui panels │      │ (same binary) │  │
                        │  └──────┬──────┘      └──────┬────────┘  │
                        │         │  CommandStack     │            │
                        │         ▼                   ▼            │
                        │  ┌──────────────────────────────┐        │
                        │  │   spark engine (library)     │        │
                        │  │  App loop · Time · Input     │        │
                        │  │  World (hecs) + Scenes (RON) │        │
                        │  │  Rules (event→action)        │        │
                        │  │  Physics (rapier 2D+3D)      │        │
                        │  │  Audio (rodio)               │        │
                        │  │  Assets (registry + cache    │        │
                        │  │           + notify watcher)  │        │
                        │  │  Renderer (wgpu) ────────────┼─► GPU  │
                        │  │   ├─ sprite pass (2D)        │        │
                        │  │   ├─ PBR mesh pass (3D)      │        │
                        │  │   └─ egui pass (UI/HUD)      │        │
                        │  └──────────────────────────────┘        │
                        └──────────────────────────────────────────┘
```

### 4.1 Unified 2D/3D rendering

One `Renderer`, one frame graph. Each frame produces a `FrameDraw` structure containing
sprite instances (instanced quads, camera-space) and mesh instances (instanced PBR,
world-space). The 3D pass runs first with depth; the 2D pass runs on top (sprites are
world-positioned with a Z coordinate — layering is free). One material type covers both:
albedo texture/color + emissive + (metallic/roughness for meshes). Cameras are either
orthographic (2D) or perspective (3D); the scene picks one primary camera. Directional
shadow mapping applies to the 3D pass only.

**Rejected:** two renderers (duplicated pipelines, duplicated materials, duplicated bugs).

### 4.2 Reflection + code generation (the LOC engine)

A component declares itself once:

```rust
#[derive(ComponentDef, Clone, Serialize, Deserialize)]
struct Sprite { image: AssetRef, color: Color, size: Vec2, #[inspector(skip)] flip: bool }
```

`spark_macros` generates: the egui inspector body (per-field widgets), and registration
metadata. A single generic `Registry::register::<T>()` then provides, for *every*
component: scene (de)serialization, editor inspection, cloning, and "add component" menus.
Cost ≈ 450 LOC of macro code; saves ≈ 25–40 LOC per component forever, and keeps user
game components first-class with zero boilerplate. This is the primary mechanism that
makes the 10k-LOC budget realistic rather than aspirational.

### 4.3 Rules: data-driven behavior (decided over an interpreter)

The user choice. Scene entities carry `rules: Vec<Rule>`; a `Rule` is
`on: Event, when: [Condition], do: [Action]`:

```ron
(
  on: CollisionEnter(tag: "coin"),
  when: [Cooldown(0.1)],
  do: [DestroyOther, AddVar("coins", 1), PlaySound("sfx/coin.ogg"), Log("got one")]
)
```

| | Rules (chosen) | Mini-language interpreter |
|---|---|---|
| Engine LOC | ~650 | ~1,300–1,800 (lexer/parser/interp/binders) |
| Hot reload | free (scene reload already exists) | must rebuild parse trees |
| Safety | data is sandboxed by construction | interpreter bugs = engine bugs |
| Ceiling | declarative patterns only | arbitrary logic |
| Escape hatch | Rust systems via public API | same |

The ceiling is the honest cost; it is mitigated by pillar 5 (Rust is always available)
and by keeping the action vocabulary open (engine + games register new actions/conditions
in one line each via the same generic registry pattern used for components).

### 4.4 Editor = engine (single binary)

`spark` with no args opens the editor; `spark --game <dir>` runs the same loop without
editor panels. Play-in-editor snapshots the scene (serialization is already universal),
runs the real simulation, and restores on stop. Undo/redo is a command stack around every
editor mutation. This removes an entire application's worth of glue code versus a separate
editor binary and guarantees WYSIWYG — there is only one engine.

### 4.5 Asset pipeline

`assets/` is watched (notify). Importers map source types (png/jpg/hdr → texture,
glb/gltf → meshes+materials, wav/ogg/mp3 → sound, ron → prefab/scene, ttf → editor font)
into a typed registry of `Handle<T>`; a content-hash cache in `.spark_cache/` avoids
re-decoding unchanged sources. File change → re-import → handles stay valid (registry
indirection) → live update in viewport. This is the entire pipeline: no external build
step, no server (constraint: no cloud services).

### 4.6 Headless mode

`App::run_headless()` runs the full simulation loop (ECS + rules + physics + audio logic)
with rendering turned off. Used by CI integration tests and by server-side/deterministic
simulation. This keeps the test suite GPU-free — a hard requirement for cloud CI runners.

---

## 5. Line-of-code budget (enforced in CI)

| Module | Budget |
|---|---|
| spark engine crate | ~5,400 |
| spark_macros (proc-macro) | ~450 |
| spark_editor | ~1,800 |
| **Total core** | **~7,600 target / 15,000 ceiling** |

Counted as non-blank, non-comment lines of Rust in `crates/*/src` excluding `#[cfg(test)]`
blocks and `tests/` directories, plus WGSL shader lines reported separately. The counting
script (`tools/count_loc.py`) runs in CI and fails the build above the ceiling.
Demo games and templates contain **no Rust** — they are pure data, which is the point.

---

## 6. Accepted trade-offs (explicit, not silent)

1. **In-game text rendering uses the egui overlay** in 0.1 rather than a custom glyph
   pipeline (−600 LOC). Upgrade path: ab_glyph-based text sprites on the roadmap.
2. **glTF skinning/animations** are imported as metadata but not evaluated in 0.1;
   static meshes + node transforms + materials come in. (Documented in ROADMAP.)
3. **One directional shadow-casting light**; point lights are unshadowed. PCF 3×3.
4. **Positional audio** deferred; rodio gives us channels/volume/looping cheaply.
5. **MSAA off** in 0.1 (wgpu setup kept ready), FXAA-style AA on the roadmap.
6. Export produces a *packaged runtime* (binary + assets), not a per-game recompile;
   games needing native code depend on the `spark` library and build with cargo directly.

---

## 7. Platform & build policy

- Supported: **Windows 10/11** (DX12/Vulkan), **Linux** (Vulkan primary, GLES fallback),
  x86_64. CI: `windows-latest` + `ubuntu-24.04`.
- Linux build deps: `libasound2-dev` (audio), `libudev-dev` (gamepad) — installed by CI;
  no other system packages required.
- No cloud services, no telemetry, no network at runtime.
- License: **MIT OR Apache-2.0** (dual), matching the ecosystem norm for maximum reuse.
