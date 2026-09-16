---
name: release
description: Cuts a stable spacetrace-hub release end to end — version bump, changelog, tag, then verifying the published archives and container images rather than trusting CI. Use when a hub release is being cut or asked about, including Turkish phrasings like "hub sürümü yayınla", "hub sürüm kes", "0.6.0 yayınla", and when someone asks whether a hub version has shipped yet or what is waiting to be released.
---

# Release — spacetrace-hub

> **This skill can fire on its own.** Run the reading and verification steps
> without asking; but **commit, tag and push cannot be undone** — say what you
> are about to do and get approval before those three. Without approval the
> release is not cut, it is prepared.

Full prose in [RELEASING.md](../../../RELEASING.md). This is the order of
operations plus the things that have actually gone wrong.

**Archives are published into the public core repository**
(`unalcakir28/spacetrace`); **container images go to GHCR.**

## 1. Decide what is being released

```bash
git log --oneline $(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)..HEAD
```

Then read the unreleased hub entries in the **core** repository's
`crates/changelog/changelog.json`. If a user-visible change in this range has no
entry, stop and write it first — five locales, in the core repo.

## 2. Close the changelog and regenerate

From a core checkout:

```bash
cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md
```

`tests/changelog.rs` asserts this file is current. **The release notes are sliced
out of this file with `awk`, not generated** — deliberately, because this file
comes from the *pinned* changelog crate, so slicing guarantees the notes match
the changelog compiled into the binary being shipped. Running the generator
against the core's `main` would not. A tagged release whose section is empty is a
hard error.

## 3. Bump the version

One place only: `version` in `Cargo.toml`. Then refresh the lockfile:

```bash
cargo check
```

**The `meta` job hard-fails on a real tag push if the tag name is not
`v` + that version** — it will not ship a binary that lies about its own version.
Dispatch tags are exempt, because `v0.1.0-test` is supposed to disagree.

## 4. Preflight

Run the `preflight` skill, including the `--locked` build in step 5 — that is
what the release actually does.

## 5. Commit, tag, push

```bash
git add -A && git commit   # subject: "0.6.0: <one line about what it is>"
git tag v0.6.0
git push origin main
git push origin v0.6.0
```

Ask before pushing. The tag push is what publishes.

Note that `paths-ignore` excludes `README.md`, `RELEASING.md` and `tasks/**` but
**deliberately not `CHANGELOG.md`** — it is the release notes, not docs. If a
push somehow needs to bypass the filter, the documented escape hatch is a manual
`workflow_dispatch` with `publish: true`.

## 6. Verify the published artefacts — do not trust green CI

- **Four archives plus `SHA256SUMS`**, named
  `spacetrace-hub-<version>-<target>.tar.gz` for the two musl and two Darwin
  targets. The names are a contract the download page links to directly.
- **Each archive contains the binary, `README.md` and `docker-compose.yml`.**
- **The binary reports its own version and commit.** Musl builds are
  cross-compiled, and the stamp only reaches them through `Cross.toml`'s
  passthrough list — if that list is wrong the binary builds fine and reports
  nothing:
  ```bash
  ./spacetrace-hub --version
  ```
- **Both architectures of the image exist**, `:v<x>` and `:latest`:
  ```bash
  docker manifest inspect ghcr.io/unalcakir28/spacetrace-hub:latest
  ```
  The image is built from the **pre-compiled** musl binaries via
  `.github/docker/Dockerfile.release`, not from the root `Dockerfile` — that one
  exists only so `docker build .` works in a clone.
- **GitHub's "latest" did not move.** Stable releases pass `--latest=false`: the
  core repo's release list holds all three components and that endpoint belongs
  to the CLI. A hub tag in the slot parses as no version and `spacetrace update`
  goes quiet permanently — measured on 9 September 2026, when hub-v0.3.0 took it.

## 7. Tell the download page

The site reads tag and asset names from `src/data/releases.ts` in
`spacetrace-website`. If anything about the names changed, that is a second
commit in a second repository on the same day.

## Report

What was released, the four archive names as published, the image digests for
both architectures, and the result of each verification in step 6 — each one
checked, not assumed.
