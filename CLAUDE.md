# spacetrace hub — project notes for Claude

Fleet dashboard: take snapshot agent push, show as `(host, root)` target, make trend, send alert. axum + SQLite, **no build step** — server spit HTML direct. Scanner, tree model, snapshot store not here; come from [unalcakir28/spacetrace](https://github.com/unalcakir28/spacetrace) core repo as git dependency.

What live elsewhere not repeat here. [README.md](README.md): setup, two-credential table, **forecast honesty** rule, `src/` layout, license. [RELEASING.md](RELEASING.md): channel table, fixed archive name, why image not built from source, two manual install step. Roadmap and cross-phase decision live in core repo `TODO.md` and `docs/DECISIONS.md` (K1 English, K2 commercial license).

This file hold only what neither say and what break quiet.

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

`--config` global, default `/etc/spacetrace-hub/hub.toml`. `init` run with no config; every other subcommand load it and run **both migration**. Rust 1.85+. Binary `spacetrace-hub`, library `spacetrace_hub` — library so HTTP surface testable end to end.

Four cross-compile target: `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` by `cross`, `aarch64-apple-darwin` and `x86_64-apple-darwin` native.

**`Cross.toml` mandatory.** Version stamp (`SPACETRACE_GIT_SHA`, `SPACETRACE_BUILD_DATE`, `SPACETRACE_CHANNEL`) reach container only through its passthrough list. Without it musl binary build **unstamped**.

No `scripts/`, Makefile or `tasks/` here. `.claude/` hold three skill below, no local hook. `.playwright-mcp/` gitignored browser-automation cache, not authority.

## Core dependency: the only pin is Cargo.lock

`Cargo.toml` pull five core crate (`scan-core`, `store`, `diff`, `changelog`, `buildinfo`) from git **with no rev, branch or tag**. No path override, no `[patch]` — `../spacetrace` checkout next door **not used**.

- `cargo update` resolve **remote repo `main`**. Local commit invisible; risk sit in **pushed** one. If remote `main` ahead of locked rev, `cargo update` drag in unreviewed core change quiet. Move pin be deliberate act, and `pin-bump` skill own order.
- **CI not pass `--locked`** (`ci.yml`); `release.yml` do, and say why: without it **two build of one tag can hold different scanner code**.
- Move pin without regenerate `CHANGELOG.md` and `tests/changelog.rs` break. **That test also pin guard** — read failure as "core moved", not "changelog stale".
- Change that break core public API invisible to this repo CI. `downstream-api-guard` agent over there exist for that.

## Things that must not break

1. **Router group is the authorization.** Write route live in own group (`require_write`), read route in other; `/snapshots` take only agent token; `/health` and `/login` unauthenticated. Check inside every handler be check that get forgotten, and forget here mean **read-only account that can delete every alert rule in fleet**.

   Guard be `every_write_route_is_in_this_list` in `tests/api.rs`. It parse `src/web.rs` as text, find every `.route(…, post(…))` and fail if not in `WRITE_ROUTES` — seven route viewer must be refused, each with form body to post. Skip only `/login`, `/logout` and `/snapshots`, which not change fleet. **New POST route with no `WRITE_ROUTES` entry break suite.**
2. **Two credential not interchangeable.** Agent token can only `POST /snapshots`; person account (`viewer` | `admin`) see dashboard. Config admin token checked **first**, so no query cost and brief-dead database cannot lock out fallback path.
3. **Token stored as plain SHA-256, no KDF**, deliberate: nothing to brute-force in 256 bit of machine randomness. New token ride in redirect query string, so plaintext shown once and never written to database.
4. **Last active admin cannot be revoked.**
5. **Settings page read-only by design.** Form that write SMTP detail back put secret in database — file people copy when they move hub and attach to bug report. Neither username nor password length shown.
6. **`/settings/test` not dry run.** It connect, authenticate and send real message; each of three be way mail config can be wrong.
7. **Record alert event start cooldown.** `COOLDOWN_SECONDS` be 6 hour per `(rule, host, root)`. `collect()` both decide and record; delivery separate because slow and can fail. `record_event` take `fired_at` as parameter so cooldown compare against recorded time.
8. **Delivery failure recorded, never retried.** Hub keep queue for dead endpoint become own problem; relay be thing that get retry right.
9. **Forecast strict**: at least 3 sample, 1 day spread, `R² ≥ 0.5`, capacity measured, horizon ≤ 3650 day. Confident wrong date worse than no date. **Growth measured on scanned folder while forecast project file system free space** — not same number.
10. **Read forward-compatible**: unrecognized `alert_rules.kind` fall
    back to `FreeBelowPercent`, an unparseable `destination` to a webhook, rather
    than making the whole rule list unreadable.

## Agent ↔ hub protocol

- Body be **raw SQLite snapshot file**, byte for byte what `spacetrace-agent push` send and what `GET /scans/{id}/download` serve. Agent have no hub-specific mode; hub have no own format.
- Path: body → bounded zstd decompression → write to `tempdir()` → `Store::import_snapshot` → retention pruning, all inside `spawn_blocking`.
- **zstd decompression bounded by `max_upload_bytes`.** Body limit bound only compressed size; few kilobyte could have drained hub. Bomb test exist.
- **Duplicate detection live in core store, not here.** Incoming scan already present under `(host, root, started_at)` triple get skipped; re-push be no-op return `imported: []`.
- **`import_snapshot` be trust boundary** — only place body off network get opened. Every scan `content_hash` recomputed and compared, and mismatch fail **whole import**, not just bad scan: transaction all or nothing and one push carry exactly one scan. It return **400**, because body that not snapshot be sender fault. Why matter: flipped bit leave structurally flawless tree carrying wrong number, and dashboard sorted by urgency then put **wrong machine at top**.
- Scan id reassigned on import. `entries.id` not rewritten — it index that scan own arena and must line up with `children_start`/`children_len`.

## What deliberately does not exist

- **No rate limiting.** `X-Forwarded-For`, `X-Real-IP` and `ConnectInfo` not handled; hub never see or log client IP. Token bucket described in core CLAUDE.md be **in agent**, not here. Only body defense be `DefaultBodyLimit::max(max_upload_bytes)` on `/snapshots`.
- **No TLS.** Reverse proxy expected, stated in four file (README, `docker-compose.yml`, `config.rs`, `web.rs`). Consequence: session cookie have **no** `Secure` flag. Put TLS in front and add one.
- **No background job, scheduler or spawned worker.** Both derived piece of work run synchronous inside import request: retention inside `import` (import only thing that grow database, so that be moment it need bounding) and alert evaluation right after, same request.
- **No connection pool** — one connection per request (`foreign_keys=ON`, `busy_timeout=30s`).
- **No frontend build step.** HTML be string concatenation in `src/web.rs`; stylesheet be `const STYLE` in `src/html.rs`. No template crate, no embedded asset, no JS bundle. One chart be hand-written inline SVG polyline, because no build step to hang chart library on. README reason: "a self-hosted tool that asks you for `npm install` before it shows you a page is a worse tool."
- **No rollup table.** Fleet summary and urgency ordering derived from stored snapshot.

## Configuration

One TOML file by `--config`, with **`deny_unknown_fields` on both struct**: misspelled key be hard error, not silent default.

`db` required. `listen` default `0.0.0.0:8080`, all interface, deliberate. `max_upload_bytes` be 512 MiB. `keep_per_target` **unlimited when unset**, so database grow with no bound. Without `[smtp]` block hub send no mail; webhook rule unaffected. `smtp.password_file` override inline `password`, and `security = "none"` with username rejected — password would ride plaintext.

Admin token precedence: inline `admin_token` → `admin_token_file` → `SPACETRACE_HUB_ADMIN_TOKEN`. **Empty token file be error, not empty token**; empty inline token fall through. `serve` **refuse to start** with no admin token.

That variable be only one server read — no `envy`, no `config` crate, no `.env`. Other three be build-time stamp.

## Schema: two migrations, two versions

One SQLite file hold both core snapshot store and hub own table, and migration independent.

- `Store::open` run store migration and reject newer schema.
- `db::migrate` create `hub_meta`, `agent_tokens`, `dashboard_users`, `alert_rules`, `alert_events`. `HUB_SCHEMA_VERSION = 3`, kept in `hub_meta.schema`; newer value be hard error. **Version step run before `CREATE TABLE IF NOT EXISTS` batch**, deliberate.
- `Destination` be **URI in one column**, not nullable pair, so "exactly one destination" be shape of data. Email reuse `mailto:` scheme so existing webhook row need no conversion. Address validated with `lettre::message::Mailbox` at form time — typo rejected in front of person who can fix it, instead of blow up at 3 a.m.
- `alert_rules.created_by` nullable; NULL mean "no one name". No backfill done, because invent one would **put person name on thing they not do**. It render as em dash — empty cell read like rendering bug.

## Habits

- **UI, code and comment be English** (core K1). `<html lang="en">` fixed and About card print English of five-language changelog entry. Hub be operator tool and tool be English; site and desktop app be surface that translate same entry. **Documentation English too, this file included** (16 September 2026). `RELEASING.md` not translated yet. Commit message English going forward; existing history stay Turkish.
- **Hand-written preferred**: `constant_time_eq`, `cookie_value`, `urlencode`. **`lettre` be deliberate exception** — hostname off network, or bare CRLF in path, can end header block and get rest read as header (injected `Bcc:`); lettre reject that at type level. rustls-only, so static musl binary survive.
- `CHANGELOG.md` **generated, not hand-edited**. From core checkout: `cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md`.
- About card changelog **compiled into binary**, newest 5 version, so it read same on air-gapped machine. **`unreleased` not shown**, and its guard count `<h3>` instead of search for word: right after release `unreleased` empty, so text search stay green at exact moment regression would slip through.
- No yarn or npm. Be stingy with dependency — see "no build step".
- **Core repo `.claude/` tooling not apply here.** Changelog entry still written from core checkout; source `crates/changelog/changelog.json` be there.

## Claude tooling in this repo

| Tool | When |
| ---- | ---- |
| `preflight` (skill) | Before push; `--locked` difference and two misleading test failure written there |
| `release` (skill) | Cut release, including why note sliced with awk |
| `pin-bump` (skill) | Move core pin; order, and test failure that mean something other than what it say |

From shared `spacetrace-tools` plugin (install it, and full list, live in workspace notes and plugin README). This repo need no local hook — plugin supply all three.

| Tool | When |
| ---- | ---- |
| `security-reviewer` (agent) | **This repo only.** Two credential, router-group authorization, `import_snapshot` trust boundary, SMTP secret — threat model, not house rule |
| `core-pin-guard` (agent) | Core API diff before move pin |
| `pin-move-guard` (hook) | Warn before `cargo update` move pin |
| `block-changelog-edit` (hook) | Edit/Write on generated `CHANGELOG.md` |
| `rustfmt-on-edit` (hook) | Format edited `.rs` file |

Also `doc-drift-auditor`, `code-reviewer` and `test-writer`.

## Tests

```bash
cargo test    # no service container, live DB or network access needed
```

- `tests/api.rs` prove thing worth proving: **separation between two credential**. Harness build `Config` from inline TOML, run **both migration way `main.rs` do**, then open real socket on `127.0.0.1:0`. Everything inside `tempdir()`.
- Test body be **real snapshot**: `spacetrace_scan_core::scan` → `save` → `export_snapshot`. Repeated push scan directory **caller own**, new temporary path each time, so every push look like new machine.
- Webhook delivery watched with **real receiver** on local socket. SMTP test cover page and rule setup, not delivery.
- Cold build need **network** (git dependency) and **C toolchain**: `rusqlite` `bundled` feature compile SQLite from source, and `zstd` and lettre `ring` carry C/asm. Hence `apk add musl-dev` in root Dockerfile.
- CI be single `ubuntu-latest` job on toolchain `stable`, not 1.85. macOS leg dropped — this be server deployed on Linux — and release workflow still build and ship both darwin target.

## Releasing

[RELEASING.md](RELEASING.md) own procedure: channel table, archive name, why image not built from source, manual step. What break:

- **`v*` tag be only trigger** (19 September 2026). Push to `main` build nothing. Rolling `hub-continuous` tag and `:main` / `:edge` image gone, and `paths-ignore` gone with them: its only leftover effect would be skip *tag* push whose commit touched documentation, publish nothing. **Do not add either back.** Tagged image be `:v<x>` and `:latest`.
- **Release not finished until site rebuilt.** Site copy changelog in at build time and no longer poll for it: `gh workflow run pages.yml` in `spacetrace-website`.
- **Root Dockerfile NOT release path.** It build from source (`rust:1.85-alpine`) so `docker build .` work in clone. Real path be `.github/docker/Dockerfile.release`, which compile nothing: `COPY bin/${TARGETARCH}/spacetrace-hub` onto `alpine:3.21`.
- Image asset uploaded **as directory** (`image`), not as glob (`image/**`): glob make artifact root `image/bin/<arch>` and lose layout Dockerfile copy. **`retention-days: 1`** — it reach publish step in same run, and 90-day default once filled account 0.5 GB Actions storage and killed real release.
- **Tag/manifest guard**: on real tag push, name ≠ `v` + `Cargo.toml` version fail `meta` job, so binary that lie about own version never published. Dispatch tag exempt.
- **Binary go to core repo** with `RELEASE_TOKEN`. Without secret workflow **not fail**: it publish here and warn download page cannot pick them up. Tag be `hub-v*`.
- **`--latest=false` on stable release.** `releases/latest` in core repo belong to CLI; hub tag in that slot make `spacetrace update` stop recognise release and those install go quiet forever. hub-v0.3.0 took it on 9 September 2026.
- **Release note sliced out of `CHANGELOG.md` with `awk`, not generated.** `CHANGELOG.md` come from *pinned* changelog crate and test assert it current, so slice guarantee note match changelog compiled into binary; generate from core `main` would not. Empty section on tagged release be hard error.
- Checksum step use `find . -maxdepth 1 -type f`, because `sha256sum ./*` die on first directory — that be how stray folder inside asset take down release.

## Numbers go stale

README HTTP API table went stale quiet once: after `/people`, `/settings`, `/about` and POST route added it still described old surface and labeled read group "admin", while code allow every logged-in role. Test count said 94 when real number be 123. **Update table when you add route and count when you add test, same commit.** Both turn wrong where nobody look.
