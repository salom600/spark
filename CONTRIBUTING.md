# Contributing to spark

Thanks for helping build a small engine that stays small.

## Ground rules

1. **The budget is a feature.** Core (engine + macros + editor) must stay
   under 10,000 non-blank, non-comment lines — the ambitious target — with
   15,000 as the CI ceiling. Run `python3 tools/count_loc.py` before you open
   a PR. If your feature adds 500 lines, show what it multiplies or replaces.
2. **Data over code.** Ask first whether the change can be a rule, a
   component, or data instead of engine logic. New `Action`/`Cond` variants
   are usually better than new systems.
3. **No new mandatory dependencies.** Anything the engine *must* link counts
   against everyone's build. A strong case needs either massive LOC savings
   or a capability we cannot reasonably own.
4. **Everything tests headless.** CI has no GPU and no audio device; engine
   code must not require a window to be tested. If it renders, test the data
   path; if it plays sound, test the logic path.

## Development setup

```bash
git clone https://github.com/salom600/spark.git
cd spark
cargo test --workspace                          # 21 tests must pass
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python3 tools/count_loc.py
```

Linux needs `libasound2-dev libudev-dev`; see [BUILDING.md](BUILDING.md).

## What CI runs on every PR

Ubuntu 24.04 + Windows: build all targets, all tests, clippy with
`-D warnings`, rustfmt check, and the line-count ceiling. Keep it green —
there are no style exceptions.

## Pull requests

- One logical change per PR; describe *why* in terms of the design pillars
  (see [DECISIONS.md](DECISIONS.md) §1).
- New components: demonstrate the derive macro path (no hand-written
  inspectors).
- New rule vocabulary: add a unit test in `rules.rs` and mention it in
  `docs/ARCHITECTURE.md`.
- Bug fixes: include a failing-test-first commit when practical.
- Docs count as contributions — `DECISIONS.md` entries for any new trade-off
  are expected.

## Adding a component (the 3-line contract)

```rust
#[derive(spark_macros::ComponentDef, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Health { pub value: f32, pub max: f32 }
// engine or game: registry.register::<Health>();
```

That is the whole integration: scenes save it, the inspector edits it,
duplicate/undo handle it. If you find yourself writing more, something is
wrong — open an issue.

## Adding a rule action

1. Add the variant to `Action` in `rules.rs`.
2. Implement it in `run_action` (defer structural world changes via the
   queues already provided).
3. Add a test in `rules.rs` tests + a catalogue entry in the editor's action
   picker (`panels.rs`).
4. One line in `docs/ARCHITECTURE.md`'s action list.

## License

By contributing you agree your work is dual-licensed MIT OR Apache-2.0, at
the recipients' choice, matching the repository.
