//! Routes, authentication and the pages themselves.
//!
//! Two audiences with different credentials, deliberately not interchangeable:
//!
//! * **Agents** hold a token from the Agents page and may do exactly one thing,
//!   `POST /snapshots`. An agent token cannot read the dashboard, so a token
//!   sitting in a config file on a NAS is not a key to the whole fleet.
//! * **A person** holds the admin token from the config and may read
//!   everything. That is a separate credential on purpose.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Form, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::Connection;
use serde::Deserialize;
use spacetrace_store::Store;

use crate::alerts;
use crate::config::Config;
use crate::db::{self, AlertKind};
use crate::fleet;
use crate::html::{self, escape};

/// Cookie holding the admin token after a successful sign-in.
const SESSION_COOKIE: &str = "st_hub";

pub struct AppState {
    db: PathBuf,
    admin_token: String,
    max_upload_bytes: usize,
    keep_per_target: Option<usize>,
    client: reqwest::Client,
    started: Instant,
}

impl AppState {
    /// The snapshot store. Opened per request: SQLite in WAL mode handles
    /// concurrent readers, and a connection is cheap next to the work done
    /// with it.
    fn store(&self) -> Result<Store> {
        if let Some(parent) = self.db.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
        }
        Store::open(&self.db)
    }

    /// A connection for the hub's own tables, in the same file.
    fn conn(&self) -> Result<Connection> {
        let conn =
            Connection::open(&self.db).with_context(|| format!("opening {}", self.db.display()))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        Ok(conn)
    }
}

pub fn router(config: &Config, admin_token: String) -> Result<Router> {
    let state = Arc::new(AppState {
        db: config.db.clone(),
        admin_token,
        max_upload_bytes: config.max_upload_bytes,
        keep_per_target: config.keep_per_target,
        client: reqwest::Client::builder()
            .build()
            .context("building the HTTP client")?,
        started: Instant::now(),
    });

    // Reachable without any credential: liveness, and the sign-in form.
    let public = Router::new()
        .route("/health", get(health))
        .route("/login", get(login_page).post(login_submit));

    // Agents. Only this one route, and only with an agent token.
    let ingest = Router::new()
        .route("/snapshots", post(receive_snapshot))
        .layer(DefaultBodyLimit::max(config.max_upload_bytes))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_agent_token,
        ));

    let dashboard = Router::new()
        .route("/", get(fleet_page))
        .route("/target", get(target_page))
        .route("/alerts", get(alerts_page).post(create_alert))
        .route("/alerts/delete", post(delete_alert))
        .route("/tokens", get(tokens_page).post(create_agent_token))
        .route("/tokens/revoke", post(revoke_agent_token))
        .route("/logout", post(logout))
        .route("/about", get(about_page))
        .route("/api/fleet", get(api_fleet))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    Ok(public.merge(ingest).merge(dashboard).with_state(state))
}

pub async fn serve(config: &Config, admin_token: String) -> Result<()> {
    let app = router(config, admin_token)?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    eprintln!(
        "spacetrace-hub listening on http://{}",
        listener.local_addr()?
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http server failed")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("shutting down");
}

// -------------------------------------------------------------------- auth

/// Compare without an early exit on the first differing byte.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Read one cookie out of the header without pulling in a cookie library.
pub fn cookie_value<'a>(header_value: &'a str, name: &str) -> Option<&'a str> {
    header_value.split(';').find_map(|pair| {
        let pair = pair.trim();
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then_some(value.trim())
    })
}

async fn require_admin(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let from_cookie = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE));
    let presented = bearer(&request).or(from_cookie).unwrap_or("");

    if constant_time_eq(presented.as_bytes(), state.admin_token.as_bytes()) {
        return next.run(request).await;
    }

    // A browser gets the sign-in page; an API client gets a status it can act
    // on rather than a redirect to HTML.
    let wants_html = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));

    if wants_html {
        Redirect::to("/login").into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({ "error": "admin token required" })),
        )
            .into_response()
    }
}

async fn require_agent_token(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = bearer(&request).unwrap_or("").to_string();
    let conn = match state.conn() {
        Ok(conn) => conn,
        Err(err) => return failure(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
    };

    match db::verify_token(&conn, &presented) {
        Ok(Some(_)) => next.run(request).await,
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({ "error": "unknown or revoked agent token" })),
        )
            .into_response(),
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

// ------------------------------------------------------------------ public

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        // Which build, not just which version. This endpoint needs no
        // credential, so it is the one thing an operator can read off a
        // container they are only half sure about — and every continuous build
        // reports the same version number.
        "commit": spacetrace_buildinfo::GIT_SHA,
        "channel": spacetrace_buildinfo::CHANNEL,
    }))
}

#[derive(Deserialize)]
struct LoginForm {
    token: String,
}

async fn login_page() -> Html<String> {
    Html(html::page(
        "Sign in",
        "",
        r#"<div class="card" style="max-width:420px;margin:40px auto">
<h1>Sign in</h1>
<p class="lede">Paste the admin token from the hub's configuration.</p>
<form method="post" action="/login">
<label>Admin token<input type="password" name="token" autofocus autocomplete="current-password"></label>
<button type="submit">Sign in</button>
</form>
<p class="hint" style="margin-top:14px">Agent tokens do not work here. They can
push snapshots and nothing else.</p>
</div>"#,
    ))
}

async fn login_submit(State(state): State<Arc<AppState>>, Form(form): Form<LoginForm>) -> Response {
    if !constant_time_eq(form.token.trim().as_bytes(), state.admin_token.as_bytes()) {
        return Html(html::page(
            "Sign in",
            "",
            r#"<div class="card" style="max-width:420px;margin:40px auto">
<h1>Sign in</h1>
<p class="lede" style="color:#fca5a5">That token was not accepted.</p>
<form method="post" action="/login">
<label>Admin token<input type="password" name="token" autofocus></label>
<button type="submit">Sign in</button>
</form></div>"#,
        ))
        .into_response();
    }

    // HttpOnly so script cannot read it, SameSite=Strict so another site
    // cannot ride it. Add `Secure` by terminating TLS in front of the hub.
    let cookie = format!(
        "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000",
        form.token.trim()
    );
    (
        StatusCode::SEE_OTHER,
        [(header::SET_COOKIE, cookie), (header::LOCATION, "/".into())],
    )
        .into_response()
}

async fn logout() -> Response {
    (
        StatusCode::SEE_OTHER,
        [
            (
                header::SET_COOKIE,
                format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
            ),
            (header::LOCATION, "/login".into()),
        ],
    )
        .into_response()
}

// ------------------------------------------------------------------ ingest

/// Accept a snapshot pushed by an agent.
///
/// The body is exactly what `spacetrace-agent push` sends and what
/// `GET /scans/{id}/download` serves, so an agent needs no hub-specific mode.
async fn receive_snapshot(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let compressed = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("zstd"));

    let limit = state.max_upload_bytes;
    let cloned = Arc::clone(&state);
    let imported =
        tokio::task::spawn_blocking(move || import(&cloned, &body, compressed, limit)).await;

    let imported = match imported {
        Ok(Ok(ids)) => ids,
        Ok(Err(err)) => return failure(err.0, &err.1),
        Err(err) => {
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("import task failed: {err}"),
            )
        }
    };

    // Evaluate alerts against the new state. Done after the import so the
    // decision is made on what was actually stored.
    let fired = evaluate_alerts(&state).await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "imported": imported,
            "alerts_fired": fired,
        })),
    )
        .into_response()
}

type WebError = (StatusCode, String);

fn import(
    state: &AppState,
    body: &[u8],
    compressed: bool,
    limit: usize,
) -> Result<Vec<i64>, WebError> {
    let raw = if compressed {
        // The body limit only bounds the compressed size, so decompression
        // has to be bounded too or a few kilobytes could exhaust the hub.
        decode_zstd_bounded(body, limit).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("body is not valid zstd: {e}"),
            )
        })?
    } else {
        body.to_vec()
    };

    let dir =
        tempfile::tempdir().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = dir.path().join("incoming.sqlite");
    std::fs::write(&path, &raw).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut store = state
        .store()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let imported = store
        .import_snapshot(&path)
        // A body that is not a snapshot is the sender's mistake.
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    // Retention runs here rather than on a timer: the only thing that grows
    // the database is an import, so this is exactly when it needs bounding.
    if let Some(keep) = state.keep_per_target {
        for id in &imported {
            if let Ok(Some(meta)) = store.scan(*id) {
                let _ = store.prune_target(&meta.root, &meta.host, keep);
            }
        }
    }
    Ok(imported)
}

fn decode_zstd_bounded(data: &[u8], limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let decoder = zstd::stream::Decoder::new(data)?;
    let mut out = Vec::new();
    decoder.take(limit as u64 + 1).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("decompressed body exceeds the {limit} byte limit"),
        ));
    }
    Ok(out)
}

/// Fire whatever the new snapshots warrant. Returns how many were delivered.
async fn evaluate_alerts(state: &AppState) -> usize {
    let Ok(store) = state.store() else { return 0 };
    let Ok(targets) = fleet::targets(&store) else {
        return 0;
    };
    let Ok(conn) = state.conn() else { return 0 };

    let firings = match alerts::collect(&conn, &targets, db::now_unix()) {
        Ok(firings) => firings,
        Err(err) => {
            eprintln!("could not evaluate alerts: {err:#}");
            return 0;
        }
    };

    let mut delivered = 0;
    for (event_id, firing) in firings {
        eprintln!("alert: {}", firing.message);
        match alerts::deliver(&state.client, &firing).await {
            Ok(detail) => {
                delivered += 1;
                let _ = db::mark_delivered(&conn, event_id, Some(&detail));
            }
            Err(detail) => {
                eprintln!("  webhook failed: {detail}");
                let _ = db::mark_failed(&conn, event_id, &detail);
            }
        }
    }
    delivered
}

// --------------------------------------------------------------- dashboard

async fn fleet_page(State(state): State<Arc<AppState>>) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(err) => return error_page("Fleet", &format!("{err:#}")),
    };
    let targets = match fleet::targets(&store) {
        Ok(targets) => targets,
        Err(err) => return error_page("Fleet", &format!("{err:#}")),
    };
    let summary = fleet::summarise(&targets);
    let now = db::now_unix();

    let mut body = String::new();
    body.push_str("<h1>Fleet</h1><p class=\"lede\">");
    if targets.is_empty() {
        body.push_str("No snapshots yet. Point an agent at this hub with <code>spacetrace-agent push</code>.</p>");
        body.push_str(&format!(
            r#"<div class="card"><h2 style="margin-top:0">Getting started</h2>
<p class="hint">Create an agent token on the <a href="/tokens">Agents</a> page, then on each machine:</p>
<pre>spacetrace-agent --config /etc/spacetrace/agent.toml \
  push {host} --token &lt;agent token&gt;</pre>
<p class="hint">Add it to the agent's schedule and the fleet fills itself in.</p></div>"#,
            host = "https://your-hub.example.com"
        ));
        return Html(html::page("Fleet", "/", &body)).into_response();
    }

    body.push_str(
        "Most urgent first: what fills up soonest, then what is tight, then what is growing.</p>",
    );

    body.push_str(&format!(
        r#"<div class="grid">
<div class="stat"><div class="k">Machines</div><div class="v num">{hosts}</div><div class="s">{targets} targets</div></div>
<div class="stat"><div class="k">Snapshots</div><div class="v num">{snapshots}</div><div class="s">newest {newest}</div></div>
<div class="stat"><div class="k">Tracked</div><div class="v num">{size}</div><div class="s">sum of latest scans</div></div>
<div class="stat"><div class="k">Needs attention</div><div class="v num">{attention}</div><div class="s">{tight} tight · {soon} filling soon</div></div>
</div>"#,
        hosts = summary.hosts,
        targets = summary.targets,
        snapshots = html::count(summary.snapshots as u64),
        newest = summary
            .newest_scan
            .map(|at| escape(&html::relative(at, now)))
            .unwrap_or_else(|| "—".into()),
        size = html::bytes(summary.total_size),
        attention = summary.tight + summary.filling_soon,
        tight = summary.tight,
        soon = summary.filling_soon,
    ));

    body.push_str(
        r#"<table><thead><tr>
<th>Host</th><th>Root</th><th class="r">Size</th><th class="r">Growth</th>
<th class="r">Free</th><th class="r">Fills in</th><th class="r">Scans</th><th>Last seen</th>
</tr></thead><tbody>"#,
    );

    for target in &targets {
        let free = target.free_fraction();
        let bar_class = match free {
            Some(f) if f < 0.10 => "bar hot",
            Some(f) if f < 0.25 => "bar warm",
            _ => "bar",
        };
        let free_cell = match free {
            Some(f) => format!(
                r#"{pct:.0}%<div class="{bar_class}"><span style="width:{width:.1}%"></span></div>"#,
                pct = f * 100.0,
                width = (f * 100.0).clamp(0.0, 100.0),
            ),
            None => "—".into(),
        };
        let rate = target.bytes_per_day();
        let rate_class = match rate {
            Some(r) if r > 1024.0 => " class=\"up\"",
            Some(r) if r < -1024.0 => " class=\"down\"",
            _ => "",
        };

        body.push_str(&format!(
            r#"<tr>
<td>{host}</td>
<td><a href="/target?host={host_q}&amp;root={root_q}">{root}</a></td>
<td class="r num">{size}</td>
<td class="r num"{rate_class}>{rate}</td>
<td class="r num">{free_cell}</td>
<td class="r num">{fills}</td>
<td class="r num">{scans}</td>
<td>{seen}</td>
</tr>"#,
            host = escape(&target.host),
            host_q = escape(&urlencode(&target.host)),
            root_q = escape(&urlencode(&target.root)),
            root = escape(&target.root),
            size = html::bytes(target.latest.total_size),
            rate_class = rate_class,
            rate = escape(&html::rate(rate)),
            free_cell = free_cell,
            fills = escape(&html::horizon(target.days_until_full)),
            scans = target.snapshots,
            seen = escape(&html::relative(target.latest.started_at, now)),
        ));
    }
    body.push_str("</tbody></table>");
    body.push_str(
        r#"<p class="hint">Growth is of the scanned folder. "Fills in" projects the
filesystem's remaining space at that rate, and is left blank when the history is
too short or too noisy to extrapolate from.</p>"#,
    );

    Html(html::page("Fleet", "/", &body)).into_response()
}

#[derive(Deserialize)]
struct TargetQuery {
    host: String,
    root: String,
}

async fn target_page(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TargetQuery>,
) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(err) => return error_page("Target", &format!("{err:#}")),
    };
    let found = match fleet::target(&store, &query.host, &query.root) {
        Ok(found) => found,
        Err(err) => return error_page("Target", &format!("{err:#}")),
    };
    let Some(target) = found else {
        return error_page(
            "Target",
            &format!(
                "No snapshots of {} on {}.",
                escape(&query.root),
                escape(&query.host)
            ),
        );
    };
    let now = db::now_unix();

    let mut body = format!(
        r#"<h1>{root}</h1><p class="lede">on <strong>{host}</strong> · {scans} snapshots · last seen {seen}</p>"#,
        root = escape(&target.root),
        host = escape(&target.host),
        scans = target.snapshots,
        seen = escape(&html::relative(target.latest.started_at, now)),
    );

    let trend_note = match target.trend {
        Some(t) if t.is_reliable() => format!(
            "{} over {:.0} days, fit {:.2}",
            escape(&html::rate(Some(t.bytes_per_day))),
            t.span_days,
            t.r2
        ),
        Some(t) => format!(
            "not extrapolated: {} samples over {:.1} days, fit {:.2}",
            t.samples, t.span_days, t.r2
        ),
        None => "not enough history".into(),
    };

    body.push_str(&format!(
        r#"<div class="grid">
<div class="stat"><div class="k">Latest size</div><div class="v num">{size}</div><div class="s">{files} files</div></div>
<div class="stat"><div class="k">Filesystem</div><div class="v num">{free}</div><div class="s">free of {total}</div></div>
<div class="stat"><div class="k">Growth</div><div class="v num">{rate}</div><div class="s">{trend_note}</div></div>
<div class="stat"><div class="k">Fills in</div><div class="v num">{fills}</div><div class="s">at the current rate</div></div>
</div>"#,
        size = html::bytes(target.latest.total_size),
        files = html::count(target.latest.files),
        free = target
            .latest
            .fs_available
            .map(html::bytes)
            .unwrap_or_else(|| "—".into()),
        total = target
            .latest
            .fs_total
            .map(html::bytes)
            .unwrap_or_else(|| "not measured".into()),
        rate = escape(&html::rate(target.bytes_per_day())),
        trend_note = trend_note,
        fills = escape(&html::horizon(target.days_until_full)),
    ));

    body.push_str(&sparkline(&target));

    body.push_str("<h2>History</h2><table><thead><tr><th class=\"r\">#</th><th>Taken</th><th class=\"r\">Size</th><th class=\"r\">Change</th><th class=\"r\">Free</th><th>Label</th></tr></thead><tbody>");

    // Newest first, which is the order people read a history in.
    let metas = match store.list() {
        Ok(list) => list,
        Err(err) => return error_page("Target", &format!("{err:#}")),
    };
    let mine: Vec<_> = metas
        .into_iter()
        .filter(|m| m.host == target.host && m.root == target.root)
        .collect();

    for (index, meta) in mine.iter().enumerate() {
        let change = mine
            .get(index + 1)
            .map(|older| meta.total_size as i64 - older.total_size as i64);
        let change_cell = match change {
            Some(delta) if delta > 0 => {
                format!("<span class=\"up\">{}</span>", escape(&html::delta(delta)))
            }
            Some(delta) if delta < 0 => {
                format!(
                    "<span class=\"down\">{}</span>",
                    escape(&html::delta(delta))
                )
            }
            Some(_) => "±0".into(),
            None => "—".into(),
        };
        body.push_str(&format!(
            r#"<tr><td class="r">{id}</td><td>{taken}</td><td class="r num">{size}</td>
<td class="r num">{change}</td><td class="r num">{free}</td><td>{label}</td></tr>"#,
            id = meta.id,
            taken = escape(&html::timestamp(meta.started_at)),
            size = html::bytes(meta.total_size),
            change = change_cell,
            free = meta
                .fs_free_fraction()
                .map(|f| format!("{:.0}%", f * 100.0))
                .unwrap_or_else(|| "—".into()),
            label = escape(meta.label.as_deref().unwrap_or("")),
        ));
    }
    body.push_str("</tbody></table>");

    // What actually grew, between the two most recent snapshots.
    if mine.len() >= 2 {
        body.push_str("<h2>What changed most recently</h2>");
        match diff_last_two(&store, &mine[1].id, &mine[0].id) {
            Ok(rows) if rows.is_empty() => {
                body.push_str("<div class=\"empty\">Nothing changed by more than a megabyte.</div>")
            }
            Ok(rows) => {
                body.push_str("<table><thead><tr><th class=\"r\">Change</th><th>Status</th><th class=\"r\">Now</th><th>Path</th></tr></thead><tbody>");
                body.push_str(&rows);
                body.push_str("</tbody></table><p class=\"hint\">Folders that only pass a change through are skipped: the row shown is the first level where the change genuinely spreads out.</p>");
            }
            Err(err) => body.push_str(&format!(
                "<div class=\"empty\">Could not compare: {}</div>",
                escape(&format!("{err:#}"))
            )),
        }
    }

    Html(html::page(&target.root, "/", &body)).into_response()
}

fn diff_last_two(store: &Store, from: &i64, to: &i64) -> Result<String> {
    let (old_tree, _) = store.load(*from)?;
    let (new_tree, _) = store.load(*to)?;
    let report = spacetrace_diff::diff(
        &old_tree,
        &new_tree,
        &spacetrace_diff::DiffOptions {
            min_delta: 1024 * 1024,
            include_files: false,
            ..Default::default()
        },
    );

    let mut out = String::new();
    for change in report.changes.iter().take(40) {
        let class = if change.delta() >= 0 { "up" } else { "down" };
        out.push_str(&format!(
            r#"<tr><td class="r num {class}">{delta}</td><td>{kind}</td>
<td class="r num">{now}</td><td>{path}</td></tr>"#,
            class = class,
            delta = escape(&html::delta(change.delta())),
            kind = escape(&format!("{:?}", change.kind).to_lowercase()),
            now = html::bytes(change.new_size),
            path = escape(&change.path),
        ));
    }
    Ok(out)
}

/// A history sparkline as inline SVG.
///
/// Inline rather than a chart library: it is twelve lines of geometry, and the
/// hub has no frontend build step to hang a dependency off.
fn sparkline(target: &fleet::Target) -> String {
    if target.history.len() < 2 {
        return String::new();
    }
    let (width, height, pad) = (1120.0f64, 90.0f64, 6.0f64);
    let first = target.history.first().map(|p| p.at).unwrap_or(0);
    let last = target.history.last().map(|p| p.at).unwrap_or(first + 1);
    let span = ((last - first) as f64).max(1.0);
    let max = target
        .history
        .iter()
        .map(|p| p.bytes)
        .max()
        .unwrap_or(1)
        .max(1) as f64;

    let points: Vec<String> = target
        .history
        .iter()
        .map(|p| {
            let x = pad + ((p.at - first) as f64 / span) * (width - 2.0 * pad);
            let y = height - pad - (p.bytes as f64 / max) * (height - 2.0 * pad);
            format!("{x:.1},{y:.1}")
        })
        .collect();

    // r##"..."## because the stroke colour contains a `#`, which would close
    // a single-hash raw string.
    format!(
        r##"<div class="card"><svg viewBox="0 0 {width:.0} {height:.0}" width="100%" height="{height:.0}" role="img" aria-label="size over time">
<polyline fill="none" stroke="#38bdaf" stroke-width="2" points="{points}"/>
</svg><div class="hint" style="display:flex;justify-content:space-between">
<span>{from}</span><span>peak {peak}</span><span>{to}</span></div></div>"##,
        width = width,
        height = height,
        points = points.join(" "),
        from = escape(&html::timestamp(first)),
        peak = html::bytes(max as u64),
        to = escape(&html::timestamp(last)),
    )
}

// ------------------------------------------------------------------ alerts

async fn alerts_page(State(state): State<Arc<AppState>>) -> Response {
    let conn = match state.conn() {
        Ok(conn) => conn,
        Err(err) => return error_page("Alerts", &format!("{err:#}")),
    };
    let rules = db::list_rules(&conn).unwrap_or_default();
    let events = db::recent_events(&conn, 40).unwrap_or_default();
    let now = db::now_unix();

    let mut body = String::from(
        r#"<h1>Alerts</h1><p class="lede">Rules are checked whenever an agent pushes a
snapshot. A rule that has fired for a target stays quiet about it for six hours.</p>
<div class="card"><h2 style="margin-top:0">Add a rule</h2>
<form method="post" action="/alerts">
<div class="row">
<label>Host <input name="host" placeholder="any"></label>
<label>Root <input name="root" placeholder="any"></label>
<label>Condition <select name="kind">
<option value="free_below_percent">free space below … %</option>
<option value="full_within_days">fills within … days</option>
<option value="growth_above_per_day">grows more than … bytes/day</option>
</select></label>
<label>Threshold <input name="threshold" type="number" step="any" min="0" value="10" required></label>
</div>
<label>Webhook URL <input name="webhook_url" type="url" placeholder="https://…" required></label>
<button type="submit">Add rule</button>
</form>
<p class="hint">The webhook receives a JSON POST with the host, the root and a
readable message. Leave host or root empty to match everything.</p></div>"#,
    );

    body.push_str("<h2>Rules</h2>");
    if rules.is_empty() {
        body.push_str("<div class=\"empty\">No rules yet.</div>");
    } else {
        body.push_str("<table><thead><tr><th>Scope</th><th>Condition</th><th>Webhook</th><th>Added</th><th></th></tr></thead><tbody>");
        for rule in &rules {
            body.push_str(&format!(
                r#"<tr><td>{scope}</td><td>{condition}</td><td>{hook}</td><td>{added}</td>
<td class="r"><form method="post" action="/alerts/delete" style="display:inline">
<input type="hidden" name="id" value="{id}">
<button class="danger" type="submit">Delete</button></form></td></tr>"#,
                scope = escape(&format!(
                    "{}:{}",
                    rule.host.as_deref().unwrap_or("any"),
                    rule.root.as_deref().unwrap_or("any")
                )),
                condition = escape(&rule.kind.describe(rule.threshold)),
                hook = escape(&truncate(&rule.webhook_url, 44)),
                added = escape(&html::relative(rule.created_at, now)),
                id = rule.id,
            ));
        }
        body.push_str("</tbody></table>");
    }

    body.push_str("<h2>Recently fired</h2>");
    if events.is_empty() {
        body.push_str("<div class=\"empty\">Nothing has fired yet.</div>");
    } else {
        body.push_str("<table><thead><tr><th>When</th><th>Target</th><th>Message</th><th>Delivery</th></tr></thead><tbody>");
        for event in &events {
            let tag = if event.delivered {
                "<span class=\"tag ok\">delivered</span>"
            } else {
                "<span class=\"tag bad\">failed</span>"
            };
            body.push_str(&format!(
                r#"<tr><td>{when}</td><td>{target}</td><td>{message}</td><td>{tag} {detail}</td></tr>"#,
                when = escape(&html::relative(event.fired_at, now)),
                target = escape(&format!("{}:{}", event.host, event.root)),
                message = escape(&event.message),
                tag = tag,
                detail = escape(event.detail.as_deref().unwrap_or("")),
            ));
        }
        body.push_str("</tbody></table>");
    }

    Html(html::page("Alerts", "/alerts", &body)).into_response()
}

#[derive(Deserialize)]
struct AlertForm {
    host: String,
    root: String,
    kind: String,
    threshold: f64,
    webhook_url: String,
}

async fn create_alert(State(state): State<Arc<AppState>>, Form(form): Form<AlertForm>) -> Response {
    let Some(kind) = AlertKind::parse(&form.kind) else {
        return error_page("Alerts", "That is not a condition this hub understands.");
    };
    let conn = match state.conn() {
        Ok(conn) => conn,
        Err(err) => return error_page("Alerts", &format!("{err:#}")),
    };
    match db::create_rule(
        &conn,
        Some(form.host.as_str()),
        Some(form.root.as_str()),
        kind,
        form.threshold,
        &form.webhook_url,
    ) {
        Ok(_) => Redirect::to("/alerts").into_response(),
        Err(err) => error_page("Alerts", &format!("{err:#}")),
    }
}

#[derive(Deserialize)]
struct IdForm {
    id: i64,
}

async fn delete_alert(State(state): State<Arc<AppState>>, Form(form): Form<IdForm>) -> Response {
    if let Ok(conn) = state.conn() {
        let _ = db::delete_rule(&conn, form.id);
    }
    Redirect::to("/alerts").into_response()
}

// ------------------------------------------------------------------ tokens

#[derive(Deserialize)]
struct TokenQuery {
    /// Set once, right after creating a token, so it can be shown exactly once.
    #[serde(default)]
    created: Option<String>,
}

async fn tokens_page(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let conn = match state.conn() {
        Ok(conn) => conn,
        Err(err) => return error_page("Agents", &format!("{err:#}")),
    };
    let tokens = db::list_tokens(&conn).unwrap_or_default();
    let now = db::now_unix();

    let mut body = String::from(
        r#"<h1>Agents</h1><p class="lede">One token per machine. An agent token can
push snapshots and nothing else — it cannot read this dashboard.</p>"#,
    );

    if let Some(plaintext) = query.created.as_deref() {
        body.push_str(&format!(
            r#"<div class="card"><h2 style="margin-top:0">New token</h2>
<p class="lede">Copy it now. Only its hash is stored, so this is the only time it can be shown.</p>
<div class="token">{token}</div>
<p class="hint" style="margin-top:10px">On the agent:</p>
<pre>spacetrace-agent --config /etc/spacetrace/agent.toml \
  push https://your-hub.example.com --token {token}</pre></div>"#,
            token = escape(plaintext)
        ));
    }

    body.push_str(
        r#"<div class="card"><h2 style="margin-top:0">Create a token</h2>
<form method="post" action="/tokens">
<div class="row"><label>Name <input name="name" placeholder="nas, web1, db1" required></label>
<button type="submit">Create</button></div></form></div>"#,
    );

    if tokens.is_empty() {
        body.push_str("<div class=\"empty\">No agent tokens yet.</div>");
    } else {
        body.push_str("<table><thead><tr><th>Name</th><th>Created</th><th>Last push</th><th>Status</th><th></th></tr></thead><tbody>");
        for token in &tokens {
            let status = if token.revoked {
                "<span class=\"tag bad\">revoked</span>"
            } else if token.last_seen_at.is_some() {
                "<span class=\"tag ok\">active</span>"
            } else {
                "<span class=\"tag warn\">never used</span>"
            };
            let action = if token.revoked {
                String::new()
            } else {
                format!(
                    r#"<form method="post" action="/tokens/revoke" style="display:inline">
<input type="hidden" name="id" value="{}">
<button class="danger" type="submit">Revoke</button></form>"#,
                    token.id
                )
            };
            body.push_str(&format!(
                r#"<tr><td>{name}</td><td>{created}</td><td>{seen}</td><td>{status}</td><td class="r">{action}</td></tr>"#,
                name = escape(&token.name),
                created = escape(&html::relative(token.created_at, now)),
                seen = token
                    .last_seen_at
                    .map(|at| escape(&html::relative(at, now)))
                    .unwrap_or_else(|| "—".into()),
                status = status,
                action = action,
            ));
        }
        body.push_str("</tbody></table>");
    }

    body.push_str(
        r#"<p class="hint">Revoking keeps the record but stops the token working
immediately. Tokens are stored hashed, so a copy of this database is not a set of
working credentials.</p>"#,
    );

    Html(html::page("Agents", "/tokens", &body)).into_response()
}

#[derive(Deserialize)]
struct NameForm {
    name: String,
}

async fn create_agent_token(
    State(state): State<Arc<AppState>>,
    Form(form): Form<NameForm>,
) -> Response {
    let conn = match state.conn() {
        Ok(conn) => conn,
        Err(err) => return error_page("Agents", &format!("{err:#}")),
    };
    match db::create_token(&conn, &form.name) {
        // The plaintext travels in the redirect so it can be shown once and is
        // never written to the database.
        Ok((_, plaintext)) => {
            Redirect::to(&format!("/tokens?created={}", urlencode(&plaintext))).into_response()
        }
        Err(err) => error_page("Agents", &format!("{err:#}")),
    }
}

async fn revoke_agent_token(
    State(state): State<Arc<AppState>>,
    Form(form): Form<IdForm>,
) -> Response {
    if let Ok(conn) = state.conn() {
        let _ = db::revoke_token(&conn, form.id);
    }
    Redirect::to("/tokens").into_response()
}

// ------------------------------------------------------------------- misc

async fn about_page(State(state): State<Arc<AppState>>) -> Response {
    let uptime = state.started.elapsed().as_secs();
    let body = format!(
        r#"<h1>About</h1>
<div class="card">
<p><strong>spacetrace-hub {version}</strong></p>
<p class="hint">Build: <code>{commit}</code> · {channel}<br>Built: {built}<br>Database: <code>{db}</code><br>Uptime: {uptime} s</p>
</div>
{changelog}
<div class="card">
<h2 style="margin-top:0">How this fits together</h2>
<p class="hint">Agents scan their own machines and push snapshots here. The hub
stores them in the same format the agent and the command line use, so a snapshot
can be pulled back out and opened locally with no conversion.</p>
<p class="hint">Growth is measured on the scanned folder. The "fills in" forecast
projects the <em>filesystem's</em> remaining space at that rate, and is withheld
when the history is too short, too noisy, or when the filesystem's capacity was
never recorded — a confident wrong date is worse than none.</p>
<p class="hint">The hub never touches the machines it watches. It receives, it
stores, and it tells you.</p>
</div>
<form method="post" action="/logout"><button class="danger" type="submit">Sign out</button></form>"#,
        version = env!("CARGO_PKG_VERSION"),
        commit = spacetrace_buildinfo::GIT_SHA,
        channel = spacetrace_buildinfo::CHANNEL,
        built = spacetrace_buildinfo::BUILD_DATE,
        db = escape(&state.db.to_string_lossy()),
        uptime = uptime,
        changelog = changelog_card(),
    );
    Html(html::page("About", "/about", &body)).into_response()
}

/// What changed in the hub, newest first.
///
/// English, like the rest of this interface: the hub is an operator's tool and
/// the tool is English (K1). The same entries are translated on the website and
/// in the desktop app, which are the surfaces a non-English reader uses.
///
/// The entries are compiled in rather than fetched, so this page says the same
/// thing on a machine with no route to the internet — which describes a fair
/// number of the machines a hub gets installed on.
fn changelog_card() -> String {
    use spacetrace_changelog::{changelog, Component, Kind, DEFAULT_LOCALE};

    // Enough to cover "what did I just upgrade through", not the whole history.
    // The full list is a link away, and this page is not an archive.
    const SHOW: usize = 5;

    let log = changelog().component(Component::Hub);
    let mut out =
        String::from("<div class=\"card\">\n<h2 style=\"margin-top:0\">What changed</h2>\n");

    let mut section = |title: String, entries: &[spacetrace_changelog::Entry]| {
        out.push_str(&format!("<h3>{title}</h3>\n"));
        for kind in Kind::ALL {
            let matching: Vec<_> = entries.iter().filter(|e| e.kind == kind).collect();
            if matching.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "<p class=\"hint\"><strong>{}</strong></p>\n<ul>\n",
                kind.heading()
            ));
            for entry in matching {
                out.push_str(&format!(
                    "<li>{}</li>\n",
                    html::inline_code(entry.localized(DEFAULT_LOCALE))
                ));
            }
            out.push_str("</ul>\n");
        }
    };

    // `log.unreleased` is deliberately skipped. Those entries describe code
    // sitting on main, which is in no build anyone is running — announcing
    // them on the About page of a binary that does not contain them would be
    // telling the operator about a change they cannot have.
    for release in log.releases.iter().take(SHOW) {
        // A version nobody can download must not read like one they can.
        let milestone = if release.published {
            String::new()
        } else {
            " · development milestone".to_string()
        };
        section(
            format!(
                "{} — {}{}",
                escape(&release.version),
                escape(&release.date),
                milestone
            ),
            &release.entries,
        );
    }

    out.push_str(
        "<p class=\"hint\"><a href=\"https://github.com/unalcakir28/spacetrace/releases\">\
         All releases</a> · <a href=\"https://spacetrace.teknobakkall.com/changelog/\">\
         Changelog in five languages</a></p>\n</div>",
    );
    out
}

async fn api_fleet(State(state): State<Arc<AppState>>) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(err) => return failure(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
    };
    match fleet::targets(&store) {
        Ok(targets) => {
            let summary = fleet::summarise(&targets);
            Json(serde_json::json!({ "summary": summary, "targets": targets })).into_response()
        }
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
    }
}

fn error_page(title: &str, message: &str) -> Response {
    Html(html::page(
        title,
        "",
        &format!(
            r#"<div class="card"><h1>{title}</h1><p class="lede">{message}</p>
<p><a href="/">Back to the fleet</a></p></div>"#,
            title = escape(title),
            message = escape(message),
        ),
    ))
    .into_response()
}

/// Percent-encode for a query string. Written out rather than pulled in: the
/// only things encoded here are hostnames, paths and hex tokens.
pub fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The About page must not announce work that is not in this binary.
    ///
    /// Counting sections rather than searching for unreleased text keeps the
    /// test honest: `unreleased` is empty right after every release, which is
    /// exactly when a regression would otherwise pass unnoticed.
    #[test]
    fn the_about_card_lists_releases_only() {
        use spacetrace_changelog::{changelog, Component};

        let log = changelog().component(Component::Hub);
        let card = changelog_card();

        let expected = log.releases.len().min(5);
        assert_eq!(
            card.matches("<h3>").count(),
            expected,
            "one heading per shown release and nothing else; \
             `unreleased` used to add one of its own"
        );
        assert!(!card.contains("Not released yet"));
    }

    #[test]
    fn constant_time_eq_still_compares_correctly() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secre"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn cookies_are_parsed_out_of_the_header() {
        assert_eq!(cookie_value("st_hub=abc", "st_hub"), Some("abc"));
        assert_eq!(cookie_value("a=1; st_hub=abc; b=2", "st_hub"), Some("abc"));
        assert_eq!(cookie_value("a=1;st_hub=abc", "st_hub"), Some("abc"));
        assert_eq!(cookie_value("other=abc", "st_hub"), None);
        assert_eq!(cookie_value("", "st_hub"), None);
        // A cookie whose name merely contains ours must not match.
        assert_eq!(cookie_value("not_st_hub=abc", "st_hub"), None);
    }

    #[test]
    fn urlencoding_escapes_what_a_path_can_contain() {
        assert_eq!(urlencode("/var/log"), "%2Fvar%2Flog");
        assert_eq!(urlencode("host-1.example"), "host-1.example");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        // Non-ASCII must survive as UTF-8 bytes.
        assert_eq!(urlencode("ü"), "%C3%BC");
    }

    #[test]
    fn truncation_keeps_the_start_and_marks_the_cut() {
        assert_eq!(truncate("short", 10), "short");
        let out = truncate("https://example.com/a/very/long/webhook/path", 12);
        assert_eq!(out.chars().count(), 12);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_decompression_bomb_is_refused() {
        let bomb = zstd::encode_all(vec![0u8; 50 * 1024 * 1024].as_slice(), 3).unwrap();
        assert!(decode_zstd_bounded(&bomb, 1024).is_err());
        let ok = zstd::encode_all(b"hello".as_slice(), 3).unwrap();
        assert_eq!(decode_zstd_bounded(&ok, 1024).unwrap(), b"hello");
    }
}
