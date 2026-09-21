# Release

This repository is private (K2), but the binaries' download link has to be
public — a private repository's release assets can't be downloaded without
authentication. That's why `.github/workflows/release.yml` builds here, and
**publishes to the public `unalcakir28/spacetrace` repository**. The
container image, however, goes straight to GHCR.

Full rationale and the shared scheme across the three components: the core
repository's
[docs/RELEASING.md](https://github.com/unalcakir28/spacetrace/blob/main/docs/RELEASING.md).

## What happens when

| Event | Binaries | Image |
|------|----------|------|
| Push to `main` | nothing | nothing |
| Push of a `v*` tag | `hub-v*` in the public repo | `:v*`, `:latest` |
| `workflow_dispatch` | builds, does not publish — unless `publish: true` is given | — |

**A `v*` tag is the only trigger**, since 19 September 2026. A push to `main`
used to publish a rolling `hub-continuous` pre-release and the `:main` / `:edge`
images; that went when the same rolling channel was removed from all three
repositories.

There is **no `paths-ignore`**, deliberately. It only ever applied to pushes, so
with `main` gone its one remaining effect would be to skip a *tag* push whose
commit happened to touch only documentation — publishing nothing and saying
nothing. Do not put one back.

Generated archives — their names are fixed, the download page links to them
directly:

```
spacetrace-hub-<version>-x86_64-unknown-linux-musl.tar.gz
spacetrace-hub-<version>-aarch64-unknown-linux-musl.tar.gz
spacetrace-hub-<version>-aarch64-apple-darwin.tar.gz
spacetrace-hub-<version>-x86_64-apple-darwin.tar.gz
SHA256SUMS
```

Each archive contains the binary, a README and `docker-compose.yml`.

## Why the image isn't built from source

`.github/docker/Dockerfile.release` only copies the **already-built** musl
binary; buildx sets `TARGETARCH` for each platform. The `Dockerfile` at the
repo root still builds from source, and `docker build .` works in a clone —
but it's the wrong tool for a multi-arch release build: building an arm64
image that way on an amd64 runner means compiling Rust under QEMU, which
takes tens of minutes and regularly runs out of memory. The binaries are
already built at native speed with `cross`, so building a second time gains
nothing.

## Manual steps required

### 1. `RELEASE_TOKEN`

```bash
gh secret set RELEASE_TOKEN --repo unalcakir28/spacetrace-hub
```

Fine-grained PAT, **Contents: Read and write** on the
`unalcakir28/spacetrace` repository only. If the secret is missing, the
workflow does not fail — it publishes the assets to this private repo and
prints a warning; image publishing is unaffected.

### 2. Make the image public

A GHCR package created from a private repository starts out private, meaning
`docker pull` requires authentication. Once:

<https://github.com/users/unalcakir28/packages/container/spacetrace-hub/settings>
→ Change visibility → **Public**.

## Cutting a stable release

Bump `version` in `Cargo.toml`, then:

```bash
git tag v0.2.0 && git push origin v0.2.0
```
