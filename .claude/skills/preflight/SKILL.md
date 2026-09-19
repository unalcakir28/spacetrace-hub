---
name: preflight
description: Runs everything CI runs for the hub, cheapest first. Use before any push to main, after finishing a change and before committing it, after adding a route or touching the config schema, after a core pin bump, and whenever someone asks whether CI will pass — including Turkish phrasings like "push etmeden önce kontrol et", "her şey yeşil mi", "CI geçer mi".
---

# Preflight — spacetrace-hub

CI is a single `ubuntu-latest` job on stable Rust — lint and tests were merged,
and the macOS leg was dropped. Run the steps **in order** and stop at the first
failure.

Note that CI runs `cargo build` and `cargo test` **without `--locked`**; only the
release workflow passes it. So a green CI does not prove the core pin is the one
that will ship — see step 5.

## 1. Formatting

```bash
cargo fmt --all --check
```

If it fails, run `cargo fmt --all` and continue.

## 2. Clippy

```bash
cargo clippy --all-targets -- -D warnings
```

## 3. Tests

```bash
cargo test
```

No service containers, no live database, no network beyond localhost — the
integration tests bind a real socket and stand up a real webhook receiver. A cold
build does need network to fetch the core git dependencies, and a C toolchain for
`rusqlite`'s bundled SQLite, `zstd` and `ring`.

Two failures mean something other than what they say:

- **`every_write_route_is_in_this_list`** — a `POST` route was added without
  putting it in `WRITE_ROUTES`. This is the authorization guard, not a
  bookkeeping test. Decide deliberately which router group the route belongs in
  before silencing it.
- **`changelog.rs`** — usually means the core pin moved, not that the changelog
  is wrong. Regenerate from a core checkout:
  ```bash
  cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md
  ```

## 4. Config still parses

```bash
cargo run -- --config hub.toml check
```

Only if the diff touched `config.rs` or the schema. `deny_unknown_fields` is on
both structs, so a renamed key is a hard error for every existing deployment —
that is the point, and it is worth seeing before a push rather than after.

## 5. Locked build, if a release is near

```bash
cargo build --locked
```

This is what the release workflow does, and the reason is written in the
workflow: the core crates are git dependencies with no `rev`, so without
`--locked` two builds of one tag can embed different scanner code.

## 6. Core pin

Not a command. If `Cargo.lock` changed in this diff, the core moved underneath
the hub and this repo's CI does not compile the core. Run the `core-pin-guard`
agent before pushing.

## 7. Changelog

Not a command either. If an operator can observe this change, it needs an entry
in the **core** repository's `crates/changelog/changelog.json`, five locales,
then a regenerated `CHANGELOG.md` here. `CHANGELOG.md` is generated — editing it
directly is blocked.

## Report

State each step's result plainly, with the failing output when something is red.
A step that was skipped is reported as skipped, not as passed. Finish with a
one-line verdict: safe to push, or what is blocking.
