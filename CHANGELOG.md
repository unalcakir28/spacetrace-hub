<!-- Generated from crates/changelog/changelog.json in unalcakir28/spacetrace.
     Do not edit by hand. From a checkout of that repo:
       cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md -->

# Changelog

What changed in spacetrace hub, newest first.

Versions marked *development milestone* were never tagged and have no
downloadable files. They are recorded because the work happened, not
because anyone can install them.

## Unreleased

### Added

- The About page now shows which build is running (commit, build date and channel) and what changed in the last few releases.
- `/health` reports the commit and the channel next to the version, so a running container can be identified without signing in.

### Performance

- Loading a large snapshot uses less memory, following the smaller tree representation in the scanning core.

## 0.2.0 — 2026-09-08 · *development milestone*

### Added

- Downloadable binaries for Linux and macOS, and container images for both amd64 and arm64.

## 0.1.0 — 2026-09-07 · *development milestone*

### Added

- A fleet dashboard: every agent and every root in one table, ordered by which one runs out of room first.
- A growth trend and a forecast, which says plainly when there is not enough history to forecast anything.
- Threshold alerts with a cooldown, delivered by webhook, plus agent tokens that can be issued and revoked from the dashboard.
