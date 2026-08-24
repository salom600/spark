# spark architecture

This is the deep-dive companion to [DECISIONS.md](../DECISIONS.md) (which
explains *why*); this document explains *how the code is organized*.

## Crate map

```
spark_macros            #[derive(ComponentDef)]  → generated inspector + Inspect impl
      │
spark (engine core)     app · render · ecs · components · scene · rules ·
      │                 physics · assets · audio · input · cmd · project · math
      │
spark_editor            main.rs (app shell) · panels.rs (UI) · commands.rs (undo)
                        state.rs (editor camera/selection) — binary name: `spark`
```

Dependency direction is strictly downward; the editor knows the engine, never
the reverse.

## The frame

One `Engine::tick` — identical in editor play mode, exported games, and
headless CI tests:

```text
assets.update()          hot-reload file watcher, re-import changed assets
audio.update()           music loop housekeeping
physics.update(dt)       body sync → step → transform pull → collision events
  rules pass             for each entity with Rules:
                           match event → check conditions → run actions
                         (deferred spawn/destroy, cross-tick messages)
  camera follow          lerp scene camera toward CameraFollowMe target
  music autoplay         first Music component wins
  scene swap / quit      LoadScene / Quit requests honored
input.end_frame()        per-frame input edges consumed
```

Rendering is a separate concern driven by `render::build_frame_draw`, which
scrapes the `hecs::World` into a GPU-friendly `FrameDraw`:

```text
shadow pass    depth-only, directional light, all mesh instances
main pass      PBR meshes (lit, PCF shadows) → sprites on top (depth-tested)
egui pass      editor panels / game HUD
```

## Reflection: how one derive replaces thousands of lines

`#[derive(ComponentDef)]` (crates/spark_macros) generates:

1. `ComponentDef::NAME` — wire name for scene files and menus;
2. `ComponentDef::inspect` — a two-column egui grid editing each field via
   the `Inspect` trait (numbers → drag values, colors → color picker, enums →
   variant switcher + per-variant fields, nested `ComponentDef` types
   recursively);
3. `Inspect for T` — so the type can also appear *inside* other components.

The `Registry` (crates/spark/src/ecs.rs) stores one `ComponentEntry` per
registered type — a handful of function pointers (`has/save/load/remove/
add_default/inspect/duplicate`) — giving the editor and the scene serializer
type-erased control over every component. A game component is therefore one
declaration plus one `register::<T>()` call.

## Scenes

`scene::SceneData` (RON) is a plain tree of `EntityRecord`s; components are
either typed variants (pretty output for the built-ins) or
`Custom(name, raw_ron)` for anything registered later. The same records power:

- `Scene::save/load` — files,
- `spawn_record_world` — prefab instantiation,
- the editor's duplicate/delete-with-undo (records are the snapshot format),
- play-mode snapshot/restore.

Hierarchy lives in `Parent`/`Children` components, maintained only through
`ecs::set_parent`; records serialize children structurally, so entity ids
never leak into files.

## Rules engine

`rules.rs` evaluates `Rule { on, when, run }` lists owned by entities:

- **Events** — Start, Update, Timer, Key/Held/Action, CollisionEnter/Exit
  (tag-filtered), Message, Clicked.
- **Conditions** — Once, Cooldown, Var comparisons (entity/global scopes),
  KeyHeld/KeyNotHeld, Chance.
- **Actions** — 25 verbs: movement (SetVelocity/SetVelX/SetVelY/Impulse/
  Teleport/Translate), spawning (Spawn prefab, DestroySelf/Other/Tagged),
  state (SetVar/AddVar, SetVisible, SetColor), feedback (PlaySound/PlayMusic,
  Log), flow (LoadScene, SendMessage, CameraFollowMe, SetGravity, Quit).

Evaluation semantics worth knowing: all of an entity's rules see the same
input frame; **globals mutate as actions run**, so two rules reacting to one
event in the same tick observe each other's writes (this is why the gravity
demo uses two keys instead of one toggle). Actions that mutate the world
structurally (spawn/destroy) are deferred to the end of the pass. Physics
steps *before* rules, so velocity set by a rule integrates from the next tick.

## Physics

`physics.rs` wraps rapier2d/rapier3d behind one component set. The scene's
`Dimension` picks the backend; `Transform.z` is draw order in 2D. Static and
kinematic bodies are driven *from* `Transform` every tick (editor moves
them); dynamic bodies drive `Transform` *to* it. Collision events flow to the
rules system as tag-filtered `CollisionPair`s; sensors become the same
events without forces.

## Assets

`assets.rs` walks the project tree once, watches it with `notify`, and lazily
decodes on first use (PNG/JPEG/HDR → RGBA, glb/gltf → meshes + materials +
textures, WAV/OGG/MP3 → bytes). Every import caches with a version counter;
the renderer's GPU cache keys on (path, version), so a file change re-uploads
in place and handles stay valid. glTF primitives are addressed as
`model.glb#0`, embedded textures as `model.glb#tex0`.

## Editor

The editor is a state machine over the same `Engine`:

- edit mode renders through an **editor camera override** (never saved into
  the scene) with the viewport scissored to the central panel;
- play mode (F5) serializes the scene, runs the real simulation un-scissored,
  and restores the serialized snapshot on stop;
- every mutation goes through `cmd::CommandStack` (swap-based commands for
  component edits, record-snapshot commands for spawn/despawn);
- `--game <dir>` skips all of the above and runs `app::run_game`, which is
  the same loop with a built-in HUD (project name + scene globals).

## Testing strategy

- 16 unit tests run GPU-free and audio-free (CI has neither): physics
  gravity/collision/velocity, rules parse/execute, scene round-trips
  (including custom components through the registry), input edges, asset
  indexing, template creation.
- 5 integration tests boot the actual demo projects headless and assert
  gameplay: the jump rule fires, spawned boxes fall, gravity toggles,
  DestroyTagged clears, the template loads.
- The renderer is exercised by compilation + the demo projects; WGSL
  struct sizes are asserted in Rust (`instance_size_matches_wgsl`).
