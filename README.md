# spacetrace hub

The fleet view for [spacetrace](https://github.com/unalcakir28/spacetrace).
Agents scan their own machines and push snapshots here; the hub keeps the
history, works out what is growing, and tells you before a disk fills up.

Self-hosted, one binary, one SQLite file. No database server, no frontend build,
no telemetry. **It never touches the machines it watches** — it receives, it
stores, and it tells you.

## Download

**[spacetrace.teknobakkall.com/hub/](https://spacetrace.teknobakkall.com/hub/)**
— what it does and how to set it up.

```bash
docker pull ghcr.io/unalcakir28/spacetrace-hub:main   # amd64 and arm64
```

Static musl binaries for Linux (x86_64, aarch64) and macOS are on the
[download page](https://spacetrace.teknobakkall.com/download/#hub).
They are built here and published into the
[public core repository's releases](https://github.com/unalcakir28/spacetrace/releases),
because a private repository's release assets need a credential to download.
Two channels: `hub-v*` for a tagged release, `hub-continuous` for the newest
`main`. See [RELEASING.md](RELEASING.md).

## Status

Phase 4 of the roadmap. Working:

- Agents push snapshots over HTTP; the body is the same file the agent stores,
  so nothing is converted on either side
- Fleet dashboard, most urgent first: what fills up soonest, then what is tight,
  then what is growing
- Per-target history with a sparkline, and a diff of the two most recent
  snapshots showing which folder actually grew
- Growth rate and a "fills in N days" forecast, **withheld** when the history is
  too short, too noisy, or when the filesystem's capacity was never recorded
- Threshold rules delivered by webhook **or email**: free space below a
  percentage, growth above a rate, or a forecast inside a horizon
- Per-agent tokens, stored hashed, revocable

Mail credentials live in the config file, not in the database — the database is
the file you back up and the one that ends up attached to a bug report. The
Settings page shows what is configured and sends a test message through the
same path a real alert takes.

Per-person access with two roles: a **viewer** sees the whole fleet and can
change none of it, an **admin** can change anything including who else has
access. Tokens rather than passwords, deliberately — a generated 256-bit token
can be stored as a plain SHA-256 because there is nothing to brute-force, where
a human-chosen password could not be. Alert rules record who added them, and
the last admin cannot revoke themselves.

Nothing is left to do here that the hub was missing.

## Run it

```bash
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > admin-token
cat > hub.toml <<'CONF'
db = "/var/lib/spacetrace-hub/hub.sqlite"
listen = "0.0.0.0:8080"
admin_token_file = "/run/secrets/admin-token"
keep_per_target = 90
CONF
docker compose up -d
```

Open <http://localhost:8080>, sign in with the contents of `admin-token`, and
create an agent token on the **Agents** page. Then on each machine:

```bash
spacetrace-agent --config /etc/spacetrace/agent.toml \
  push https://your-hub.example.com --token <agent token>
```

Add that to the agent's schedule and the fleet fills itself in.

Without Docker: `cargo build --release`, then

```bash
spacetrace-hub --config hub.toml check     # validate before starting
spacetrace-hub --config hub.toml token nas # create an agent token
spacetrace-hub --config hub.toml serve
```

## Two credentials, on purpose

| Credential | Can do | Cannot do |
|---|---|---|
| **Agent token** | `POST /snapshots` | Read the dashboard or the API |
| **Admin token** | Read everything, manage tokens and rules | Push snapshots |

A token sitting in a config file on a NAS should not be a key to the whole
fleet's inventory, so an agent token is not a dashboard credential. The reverse
holds too: the admin token cannot inject snapshots. Both directions are covered
by tests.

Agent tokens are stored as SHA-256 hashes, so a copy of the database is not a
set of working credentials. Revoking keeps the record and stops the token
working immediately.

Put a reverse proxy with TLS in front of the hub before exposing it beyond your
own network. The session cookie is `HttpOnly` and `SameSite=Strict`; add
`Secure` by terminating TLS in front.

## Honesty about the forecast

The forecast is the feature most likely to be wrong, so it is the one held back
hardest. `fills in N days` is only shown when **all** of:

- at least 3 snapshots
- spanning at least 1 day
- with a linear fit of r² ≥ 0.5
- and the filesystem's capacity was actually measured
- and the answer is inside ten years

Disk usage is frequently not linear. A log rotation or a one-off restore will
happily produce a fitted line whose slope means nothing, and a confident wrong
date is worse than no date. When any condition fails the column is blank and the
target page says why.

Growth is measured on the **scanned folder**. The forecast projects the
**filesystem's** remaining space at that rate — those are different numbers and
the pages say which is which.

## HTTP API

| Method | Route | Credential | Purpose |
|---|---|---|---|
| GET | `/health` | none | Liveness and version |
| POST | `/snapshots` | agent | Accept a pushed snapshot |
| GET | `/api/fleet` | admin | Every target with its trend, as JSON |
| GET | `/` | admin | Fleet dashboard |
| GET | `/target?host=&root=` | admin | One target's history and latest diff |
| GET | `/alerts` | admin | Rules and what has fired |
| GET | `/tokens` | admin | Agent tokens |

`POST /snapshots` accepts `Content-Encoding: zstd`. Decompression is bounded, so
a small compressed body cannot expand into an arbitrarily large one.

## Layout

```
src/
├── main.rs     init / check / token / serve
├── config.rs   TOML configuration
├── db.rs       agent tokens, alert rules, alert events
├── fleet.rs    (host, root) targets, urgency ordering, summary
├── trend.rs    least-squares growth and the forecast, with its own limits
├── alerts.rs   threshold evaluation, cooldown, delivery
├── email.rs    SMTP, and why that one is a dependency
├── web.rs      routes, the three auth layers, the pages
└── html.rs     escaping, formatting, the shared stylesheet
```

Snapshots live in `spacetrace-store` unchanged; the hub keeps no rollup table,
so there is nothing that can drift out of step with the snapshots themselves.
The dashboard is server-rendered by hand: a self-hosted tool that needs
`npm install` before it will show you a page is a worse tool.

## Develop

```bash
cargo test                                  # 94 tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

The integration tests in `tests/api.rs` run a real hub on a real socket, push
real snapshot files through it, and stand up a real webhook receiver to observe
delivery.

## Licence

Proprietary. The scanning core, snapshot store, CLI and agent are Apache-2.0 in
the [main repo](https://github.com/unalcakir28/spacetrace); the reasoning is in
that repo's `docs/DECISIONS.md` (K2).
