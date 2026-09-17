---
name: pin-bump
description: Advances the core spacetrace pin in this repository, in the order that keeps the things that break silently from breaking. Use when the core dependency is being moved forward, before tagging a hub release, when `cargo update` is about to run, and when a build or a test breaks right after a pin move — including Turkish phrasings like "core pin'ini ilerlet", "pin'i güncelle", "core'u son hâline çek".
---

# Pin bump — spacetrace-hub

`Cargo.toml` takes five core crates from git with **no rev, branch or tag**, and
there is no path override and no `[patch]`. The only pin is `Cargo.lock`. Moving
it is a deliberate act with a fixed order; the pieces that guard it exist but
live in three different places (an agent, a hook, and a test that fails under a
misleading name). This skill is the order.

`cargo update` resolves the **remote** `main`. The `../spacetrace` checkout next
to this one is not used — local commits are invisible, and the risk comes from
what has been pushed.

## 1. Find out what is coming, before moving anything

```bash
cd ../spacetrace && git fetch && git log --oneline -20
```

Then run the **`spacetrace-tools:core-pin-guard`** agent. It reports what changed
in the core's public API between the pinned rev and the target, and separates
"breaks the build" from **"compiles and changes behaviour"** — the dangerous
class, because no CI in this repository builds the core.

Pay particular attention to anything in `store`. `import_snapshot` is this
repository's trust boundary and it is the core's code doing the work: a change to
the digest check or to the duplicate triple changes what the hub accepts off the
network, and nothing in this repository's tests would name the core as the cause.

## 2. Move the pin

```bash
cargo update -p spacetrace-scan-core -p spacetrace-store -p spacetrace-changelog
```

Three crates are named, not five: they all resolve from the same git source, so
updating these moves the whole source to the same rev. The `pin-move-guard` hook
warns here — that is expected, it is this step.

## 3. Regenerate the changelog, from a core checkout

```bash
cd ../spacetrace
cargo run -p spacetrace-changelog -- markdown --component hub \
  > ../spacetrace-hub/CHANGELOG.md
```

`CHANGELOG.md` is generated and the `block-changelog-edit` hook refuses to let it
be edited by hand; redirection is the sanctioned path.

**Do this even if you think the changelog did not move.** The changelog crate is
one of the five that just moved, and the About card's changelog is compiled into
the binary, not downloaded.

## 4. Test — and read one failure correctly

```bash
cd ../spacetrace-hub && cargo test
```

**If `tests/changelog.rs` fails, it means the core moved and step 3 was skipped
or stale.** It does not mean the changelog is wrong. That test doubles as the pin
guard — its own comment says so — and its failure message is about the changelog,
which is misleading at exactly this moment.

Note that `cargo test` here does **not** pass `--locked`, and neither does CI;
only `release.yml` does. So a green test run does not prove that the rev you just
locked is the one a release would build.

```bash
cargo build --locked --all-targets
```

Then run **`preflight`** for the rest of what CI runs.

## 5. Commit the pin as its own commit

The commit is usually `Cargo.lock` + `CHANGELOG.md`; if the core's API changed it
touches `src/` too, and then the body should say what changed rather than
leaving a reader to diff two revs.

Say which core commits came in, not just that the pin moved: the lock file
records the rev, but nobody reads a rev.
