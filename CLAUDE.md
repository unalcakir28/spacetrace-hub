# spacetrace hub — project notes for Claude

Fleet dashboard: takes the snapshots agents push, shows them as
`(host, root)` targets, derives trends, sends alerts. axum + SQLite, **no
build step** — the server emits HTML directly. The scanner, the tree model
and the snapshot store are not here: they are in the
[unalcakir28/spacetrace](https://github.com/unalcakir28/spacetrace) core
repo, pulled in as a git dependency.

I do not repeat what is already covered well: [README.md](README.md) holds
the setup, the two-credential table, the **forecast honesty** rules, the
`src/` layout and the license; [RELEASING.md](RELEASING.md) holds the
channel table, the fixed archive names, why the image is not built from
source, and the two manual install steps. The roadmap and the cross-phase
decisions are in the core repo's `TODO.md` and `docs/DECISIONS.md` files
(K1 English, K2 commercial license).

This file covers only the things that **are written in neither of them and
can break silently**.

## Commands

```bash
cargo build --all-targets                    # this is how CI builds
cargo test                                   # no service container needed
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

spacetrace-hub --config hub.toml init        # write the initial config
spacetrace-hub --config hub.toml check       # check config + db, fleet summary
spacetrace-hub --config hub.toml token nas   # agent token (printed once)
spacetrace-hub --config hub.toml serve
```

`--config` is global, defaulting to `/etc/spacetrace-hub/hub.toml`. `init`
runs without a config; every other subcommand loads the config and runs
**both migrations**. Rust 1.85+, binary name `spacetrace-hub`, library name
`spacetrace_hub` — it is exposed as a library so that the HTTP surface can
be tested end to end.

Cross-compilation covers four targets: `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl` (both via `cross`), `aarch64-apple-darwin`,
`x86_64-apple-darwin` (both native).

**`Cross.toml` is mandatory.** The version stamp (`SPACETRACE_GIT_SHA`,
`SPACETRACE_BUILD_DATE`, `SPACETRACE_CHANNEL`) only reaches the container
through the passthrough list in it; if the list is missing, the musl
binaries build **without a stamp**.

There is no `scripts/`, Makefile, `.claude/` or `tasks/`. `.playwright-mcp/`
is gitignored — it is the browser-automation cache that accumulates while
reviewing pages visually (20 screenshots, 31 accessibility snapshots, one
console log), not an authority.

## Core dependency: the only pin is in Cargo.lock

`Cargo.toml` pulls the five core crates (`scan-core`, `store`, `diff`,
`changelog`, `buildinfo`) from git **with no rev, branch or tag**. There is
no path override and no `[patch]` — the `../spacetrace` checkout next to it
is **not used**.

So the only pin is `Cargo.lock`. The consequences:

- `cargo update` resolves **the remote repo's `main`**. The `../spacetrace`
  checkout next to it plays no part in this — local commits are invisible,
  the risk comes from the **pushed** ones: if remote `main` is ahead of the
  locked rev, `cargo update` silently pulls in unreviewed core changes.
  Moving the pin is a deliberate act.
- **CI does not pass `--locked`** (`ci.yml`), only `release.yml` does, and
  the reason is written there: without it **two builds of a single tag can
  embed different scanner code**.
- If you move the pin and do not regenerate `CHANGELOG.md`,
  `tests/changelog.rs` breaks — **that test is also the pin guard**, and the
  test's own comment says so.
- A change that breaks the core's public API is invisible to this repo's CI;
  the `downstream-api-guard` agent over there exists for exactly that.

## Things that must not break

1. **The router group is the authorization itself.** Write routes sit in
   their own group (`require_write`), read routes in another, `/snapshots`
   takes only the agent token, `/health` and `/login` are unauthenticated.
   Putting a check inside every handler is something that will be forgotten,
   and forgetting it here means **a read-only account that can delete every
   alert rule in the fleet**.

   Its guard is `every_write_route_is_in_this_list` in `tests/api.rs`: it
   **parses `src/web.rs` as text**, finds every `.route(…, post(…))` and
   fails if it is not in `WRITE_ROUTES`. The allowlist holds only `/login`,
   `/logout`, `/snapshots`. **Adding a POST route without updating
   `WRITE_ROUTES` breaks the test suite.**
2. **The two credentials are not interchangeable.** The agent token can only
   do `POST /snapshots`; a person account (`viewer` | `admin`) sees the
   dashboard. The admin token from the config is checked **first** — so that
   there is no query cost and so that a briefly unreachable database cannot
   lock out the fallback path.
3. **Tokens are stored as plain SHA-256, with no KDF**, and that is
   deliberate: there is nothing to brute-force in 256 bits of machine
   randomness. A newly created token travels in the redirect query string —
   so that its plaintext is shown exactly once and is never written to the
   database.
4. **The last active admin cannot be revoked.**
5. **The settings page is read-only by design.** A form that wrote SMTP
   details back would put the secret into the database — that is the file
   people copy when they move the hub and attach to bug reports. Neither the
   username nor the length of the password is shown.
6. **`/settings/test` is deliberately not a dry run**: it connects,
   authenticates and sends a real message — each of the three is one of the
   ways a mail configuration can be wrong.
7. **Recording the alert event is what starts the cooldown.**
   `COOLDOWN_SECONDS` is 6 hours, per `(rule, host, root)`; `collect()`
   both decides and records, delivery is separate because it is slow and can
   fail. `record_event` takes `fired_at` as a parameter so that the cooldown
   is compared against the time it was recorded.
8. **A delivery failure is recorded, never retried.** A hub that keeps a
   queue for an endpoint that is gone turns into a problem of its own; the
   thing that gets retries right is the relay itself.
9. **The forecast is kept strict**: at least 3 samples, 1 day of spread,
   `R² ≥ 0.5`, capacity must have been measured, horizon ≤ 3650 days. "A
   confidently wrong date is worse than no date." And **growth is measured
   on the scanned folder while the forecast projects the file system's free
   space** — not the same number.
10. **Forward-compatible reads**: an unrecognized `alert_rules.kind` falls
    back to `FreeBelowPercent`, an unparseable `destination` falls back to a
    webhook — rather than making the whole rule list unreadable.

## Agent ↔ hub protocol

- The body is the **raw SQLite snapshot file** — byte for byte the same as
  what `spacetrace-agent push` sends and what `GET /scans/{id}/download`
  serves. The agent has no hub-specific mode, the hub has no format of its
  own.
- The path: body → bounded zstd decompression (if any) → write to
  `tempdir()` → `Store::import_snapshot` → retention pruning, all inside
  `spawn_blocking`.
- **zstd decompression is bounded by `max_upload_bytes`.** The body limit
  bounds only the *compressed* size; a few kilobytes could have exhausted
  the hub. There is a bomb test.
- **Duplicate detection lives in the core store, not here.** An incoming
  scan is skipped if it already exists under the `(host, root, started_at)`
  triple — a re-push is a no-op and returns `imported: []`.
- **`import_snapshot` is a trust boundary** and the only place where a body
  that crossed the network is opened. Every scan's `content_hash` is
  recomputed and compared; a mismatch fails **the entire import** (not "skip
  the bad one" — the transaction is all or nothing and one push carries
  exactly one scan). It comes back to the client as **400**, because a body
  that is not a snapshot is the sender's fault. Why it matters: a flipped
  bit leaves a structurally flawless tree carrying the wrong number, and on
  a dashboard sorted by urgency it puts **the wrong machine at the top**.
- Scan ids are reassigned on import; `entries.id` is not rewritten because
  it is an index into that scan's own arena and has to line up with
  `children_start`/`children_len`.

## What deliberately does not exist

- **There is no rate limiting. `X-Forwarded-For` / `X-Real-IP` /
  `ConnectInfo` are not handled.** The hub never sees the client IP and
  never logs it. The token bucket described in the core CLAUDE.md is **in
  the agent**, not here — do not confuse the two. The only body defense is
  `DefaultBodyLimit::max(max_upload_bytes)` on `/snapshots`.
- **There is no TLS.** A reverse proxy is expected, and that is written in
  four files (README, `docker-compose.yml`, `config.rs`, `web.rs`). The
  consequence: the session cookie has **no** `Secure` flag — you add it by
  putting TLS in front of it.
- **There are no background jobs, no scheduler, no spawned workers.** Both
  derived pieces of work run synchronously inside the import request:
  retention inside `import` ("import is the only thing that grows the
  database, and that is exactly the moment it needs bounding") and alert
  evaluation immediately after it, in the same request.
- **There is no connection pool** — a connection is opened per request
  (`foreign_keys=ON`, `busy_timeout=30s`).
- **There is no frontend build step.** The HTML is assembled by string
  concatenation inside `src/web.rs`, the stylesheet is embedded as
  `const STYLE` inside `src/html.rs`. No template crate, no embedded assets,
  no JS bundle. The one chart is a hand-written inline SVG polyline —
  because there is no build step to hang a chart library on. The reason is
  in the README: "a self-hosted tool that asks you for `npm install` before
  it shows you a page is a worse tool."
- **There is no rollup table**: the fleet summary and the urgency ordering
  are derived from the stored snapshots.

## Configuration

A single TOML file, via `--config`. **`deny_unknown_fields` on both
structs** — a misspelled key is a hard error, not silently ignored.

`db` is required. `listen` defaults to `0.0.0.0:8080` (all interfaces,
deliberately). `max_upload_bytes` is 512 MiB. `keep_per_target` is
**unlimited when unset** — the database grows without bound. Without an
`[smtp]` block the hub sends no mail (webhook rules are unaffected);
`smtp.password_file` overrides an inline `password`, and `security = "none"`
plus a username is rejected (the password would go in plaintext).

Admin token precedence: inline `admin_token` → `admin_token_file` → the
environment variable `SPACETRACE_HUB_ADMIN_TOKEN`. **An empty token file is
an error, not an empty token**; an empty inline token falls through to the
next source. `serve` **refuses to start** without an admin token.

The server reads **no** environment variable other than that one (no `envy`,
no `config` crate, no `.env` handling). The other three are build-time
stamps.

## Schema: two migrations, two versions

A single SQLite file holds both the core snapshot store and the hub's own
tables, and the two migrations are independent:

- `Store::open` runs the store's migration and rejects a newer schema.
- `db::migrate` creates `hub_meta`, `agent_tokens`, `dashboard_users`,
  `alert_rules`, `alert_events`; `HUB_SCHEMA_VERSION = 3`, kept in
  `hub_meta.schema`, and a newer value is a hard error. **The version steps
  run before the `CREATE TABLE IF NOT EXISTS` batch**, and that is
  deliberate.
- `Destination` is **a URI in a single column**, not a nullable pair — so
  that "exactly one destination" is the shape of the data. When email was
  added the `mailto:` scheme was reused so that existing webhook rows did
  not have to be converted. Addresses are validated with
  `lettre::message::Mailbox` at form time — so that a typo is rejected in
  front of the person who can fix it, instead of blowing up at 3 a.m.
- `alert_rules.created_by` is nullable and NULL means "when there is
  no one's name"; no backfill was done, because inventing one would **put a
  person's name on something they did not do**. It renders as an em dash
  rather than being left blank — an empty cell reads like a rendering bug.

## Habits

- **The UI, the code and the comments are English** (core K1):
  `<html lang="en">` is fixed, the About card prints the English of the
  five-language changelog entry. The hub is an operator tool and the tool is
  English; the surfaces that translate the same entries are the site and the
  desktop app. **Documentation is English too, this file included** (16 September
  2026). `RELEASING.md` has not been translated yet; commit messages are
  English going forward and the existing history stays Turkish.
- **Hand-written is preferred**: `constant_time_eq`, `cookie_value`,
  `urlencode` were written by hand. **`lettre` is a deliberate exception** —
  a hostname that came off the network, or a bare CRLF in a path, can
  terminate the header block and get the rest read as headers (an injected
  `Bcc:`); lettre rejects that at the type level. rustls-only, so that the
  static musl binary survives.
- `CHANGELOG.md` is **generated, not hand-edited**: from the core checkout,
  `cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md`.
- The About card's changelog is **compiled into the binary, not
  downloaded** — so that it reads the same on an air-gapped machine; the
  newest 5 versions. **`unreleased` is not shown**, and its guard counts
  `<h3>` rather than searching for the text "unreleased": right after every
  release `unreleased` is empty, so a test that searches for the text stays
  green at exactly the moment the regression would slip through.
- No `yarn`/npm, be stingy about adding dependencies — see "no build step"
  above.
- **The core repo's `.claude/` tooling does not apply here.** Writing a
  changelog entry is still driven from the core checkout: the source
  `crates/changelog/changelog.json` is there.

## Claude tooling that lives in this repo

| Tool | When |
|------|----------|
| `preflight` (skill) | Before a push; the `--locked` difference and two misleading test failures are written there |
| `release` (skill) | Cutting a release; including why the release notes are sliced with awk |

Both **trigger on their own** — they do not wait for you to type
`/preflight`. The commit/tag/push steps of `release` are gated on approval
in the skill's body.

The shared tooling comes from the `spacetrace-tools` plugin, with the
`spacetrace-tools:` prefix — this repo needed no **local** hook, the plugin
supplies all three. The ones that concern this repo:

| Tool | When |
|------|----------|
| `security-reviewer` (agent) | **For this repo only.** The two credentials, router group authorization, the `import_snapshot` trust boundary, SMTP secrets — a threat model, not house rules |
| `core-pin-guard` (agent) | The core API diff before moving the pin |
| `pin-move-guard` (hook) | Warns before `cargo update` moves the pin |
| `block-changelog-edit` (hook) | Edit/Write to the generated `CHANGELOG.md` |
| `rustfmt-on-edit` (hook) | Formats the edited `.rs` file |

There are also the `doc-drift-auditor`, `code-reviewer` and `test-writer`
agents. **I do not keep the full list here**, it is in the plugin's README.

The plugin
**is not in this repo**, it is in the private `spacetrace-tooling/` repo
next to it — cloning this repo does not bring it:
`claude plugin marketplace add unalcakir28/spacetrace-tooling` and then
`claude plugin install spacetrace-tools@spacetrace-tooling`. The details are
in that repo's README.

## Tests

```bash
cargo test    # no service container, live DB or network access needed
```

- `tests/api.rs` — the thing actually worth proving is **the separation
  between the two credentials**. The harness builds a `Config` from inline
  TOML and runs **both migrations the way `main.rs` does**, then opens a
  real socket on `127.0.0.1:0`. Everything is inside `tempdir()`.
- The test bodies are **real snapshots**: `spacetrace_scan_core::scan` →
  `save` → `export_snapshot`. Repeated pushes have to scan a directory
  **the caller owns** — a new temporary path each time, so every push looks
  like a new machine.
- Webhook delivery is observed by standing up a **real receiver** on a local
  socket. The SMTP tests verify the page and the rule setup, not delivery.
- A cold build needs **the network** (git dependencies) and **a working C
  toolchain**: `rusqlite` with the `bundled` feature compiles SQLite from
  source, and `zstd` and lettre's `ring` carry C/asm too. That is why the
  Dockerfile at the root does `apk add musl-dev`.
- The CI matrix is `ubuntu-latest` + `macos-latest`, toolchain `stable` (not
  1.85).

## Releasing

The full sequence is in [RELEASING.md](RELEASING.md); the parts that break
easily:

- **There are two Dockerfiles and the one at the root is NOT the release
  path.** The root one (`rust:1.85-alpine`, builds from source) is there
  only so that `docker build .` works in a clone. The real path is
  `.github/docker/Dockerfile.release`: `alpine:3.21` +
  `COPY bin/${TARGETARCH}/spacetrace-hub`, **it compiles nothing**. The
  reason is inside the file — building Rust under QEMU on an amd64 runner
  takes tens of minutes and regularly runs out of memory.
- The asset is uploaded **as a directory** (`image`), not as a glob
  (`image/**`): with a glob the artifact root becomes `image/bin/<arch>` and
  the layout the Dockerfile copies is lost.
- **Tag/manifest guard**: on a real tag push, if the name ≠ `v` + the
  `Cargo.toml` version, the `meta` job fails — so that a binary that lies
  about its own version is never published. Dispatch tags are exempt.
- **The binaries are published to the core repo** (`RELEASE_TOKEN`); if the
  secret is missing the workflow **does not fail**, it publishes to this
  repo instead and prints a warning saying the download page will not be
  able to pick them up. The tags: `hub-continuous` (deleted and recreated
  every time) and `hub-v*`.
- **`--latest=false` on stable releases.** The core's release list holds
  all three components; that endpoint belongs to the CLI, and
  `spacetrace update` parses a hub tag as if it were not a version at all —
  those installs go permanently quiet (on 9 September 2026 hub-v0.3.0 took
  the slot).
- **The release notes are sliced out of `CHANGELOG.md` with `awk`, not
  generated.** The reason: `CHANGELOG.md` is generated from the *pinned*
  changelog crate and a test asserts that it is current — slicing guarantees
  that the notes are identical to the changelog compiled into the binary.
  Running the generator against the core's `main` would not guarantee that.
  On a tagged release an empty section is a hard error.
- `paths-ignore` excludes README/RELEASING/tasks but **deliberately does not
  exclude `CHANGELOG.md`** — that is the release note itself. The filter
  applies to tag pushes too; the escape hatch is a manual
  `workflow_dispatch` with `publish: true`.
- The checksum step uses `find . -maxdepth 1 -type f`, because
  `sha256sum ./*` dies on the first directory — that is how a stray folder
  inside an asset takes down an entire release.
- Images: rolling → `:main` + `:edge`, tagged → `:v<x>` + `:latest`.

## Numbers go stale

The HTTP API table in the README went stale silently once — after
`/people`, `/settings`, `/about` and all the POST routes were added the
table was left over from before and labeled the read group "admin", while
the code allows every logged-in role. The test count too: it said "94 tests"
while the real number was 123. **Update the table when you add a route and
the count when you add a test, in the same commit** — both turn wrong where
nobody is looking.
