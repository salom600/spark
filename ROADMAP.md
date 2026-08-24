# Roadmap

spark 0.1 delivers the full vertical slice: editor, 2D/3D, physics, audio,
rules, CI. The roadmap prioritizes *removing documented v1 limits* over
adding surface area — each item below is sized against the LOC budget
(current core: ~7,000 / 15,000 ceiling).

## 0.2 — Rendering & assets polish

- [ ] Text sprites in-game (glyph atlas through the existing texture path);
      removes the egui-overlay HUD limitation for games.
- [ ] glTF skinning + animation clips (the importer already parses them;
      evaluate against `Transform` hierarchies).
- [ ] MSAA (pipeline multisample state kept ready) and/or FXAA pass.
- [ ] Sprite atlases + nine-slice; texture mipmaps.
- [ ] Shadow cascade or per-light shadow maps beyond the single directional.

## 0.3 — Editor experience

- [ ] Drag-and-drop from asset browser into the viewport (spawn at cursor).
- [ ] Transform gizmos (move/rotate/scale handles) — the biggest missing
      editor power tool.
- [ ] Multi-select + batch operations.
- [ ] Scene tabs and multi-scene editing.
- [ ] Copy/paste entities (records already serialize — needs clipboard glue).

## 0.4 — Behavior depth

- [ ] Rule sub-conditions (`All`/`Any` groups) for OR-style logic without
      rule duplication.
- [ ] Global (project-wide) rules in addition to per-entity rules.
- [ ] More actions on request: camera shake, tweens (lerp vars over time),
      particles (data-driven emitter component).
- [ ] Rule expression values (`Var + 1` instead of literals) — small Pratt
      parser, no interpreter.

## 0.5 — Runtime & platform

- [ ] winit 0.31 `run_app` port (removes the two `#[allow(deprecated)]`
      spots and the leaked-window workaround documented in DECISIONS §6).
- [ ] macOS support (wgpu/Metal path exists; needs CI runner + testing).
- [ ] Steam Deck / controller-first input mapping presets.
- [ ] Save-game API (persist scene globals + tagged entity vars to disk).

## Later / exploratory

- [ ] Deterministic lockstep networking hook (the rules system is already
      deterministic given identical inputs — xorshift `Chance` included).
- [ ] Asset bundle format (single-file export with compression).
- [ ] Rust game-code hot-reload via dylib behind a feature flag.
- [ ] WebAssembly target (wgpu/webgpu path; editor stays native).

## Non-goals

- A visual scripting graph — rules cover the declarative core; anything more
  belongs in Rust systems (the escape hatch is a first-class citizen).
- Builtin networking services, telemetry, or any cloud dependency.
- Console (non-PC) platform support.
