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

- An alert rule can send email instead of calling a webhook. Type an address where the URL goes; a rule keeps exactly one destination, so there is nothing to get out of step. Mail is configured in an `[smtp]` section of the hub config file rather than in the dashboard, because the database is the file you back up and a password in it travels with every copy. A new Settings page shows the relay, never the password, and sends a test message through the same path an alert takes — so a success there means alerts will arrive, and a failure names what is wrong before a disk fills.
- People, instead of one shared credential. A new People page issues a token per person with one of two roles: a viewer sees the whole fleet and can change none of it, an admin can change anything including who else has access. Alert rules now record who added them, which is the question a shared login could never answer. Tokens rather than passwords, on purpose — a generated token can be stored as a plain hash because there is nothing to guess, and a chosen password could not. The token in the hub's config file keeps working and stays an admin: it is the way back in, which is why the last admin cannot revoke themselves either.

## 0.4.0 — 2026-09-10

### Added

- A pushed snapshot is checked against the checksum it carries and refused with a 400 if it disagrees, instead of joining the fleet. A bit flipped in transit leaves a valid tree holding a wrong number — on a dashboard that sorts by urgency, that is the wrong machine at the top.

### Changed

- The hub's snapshot database moves to a new schema so the checksum has somewhere to live. Downgrading to an older build will no longer open the file — it says so plainly rather than misreading it. Putting a copy aside before upgrading costs nothing.

## 0.3.1 — 2026-09-10

### Changed

- The About page now lists released versions only. Entries that had landed but were in no release described work your copy does not contain.

## 0.3.0 — 2026-09-09

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
