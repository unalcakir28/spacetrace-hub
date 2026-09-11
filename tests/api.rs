//! End-to-end tests against a real hub on a real socket.
//!
//! The thing most worth proving here is the separation between the two
//! credentials: an agent token must be able to push and nothing else, and the
//! admin token must not be usable as an agent token. Everything else — status
//! codes, the import path, alert delivery — only exists once a request has been
//! through the whole stack, so these go over HTTP rather than calling handlers.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use spacetrace_hub::config::Config;
use spacetrace_hub::{db, web};
use spacetrace_scan_core::{scan, ScanOptions, ScanProgress};
use spacetrace_store::Store;
use tempfile::TempDir;

const ADMIN: &str = "admin-token-7c1f";

struct Hub {
    addr: SocketAddr,
    db: PathBuf,
    _home: TempDir,
}

impl Hub {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    /// Create an agent token the way the dashboard would.
    fn new_agent_token(&self, name: &str) -> String {
        db::create_token(&self.conn(), name).unwrap().1
    }

    fn targets(&self) -> usize {
        let store = Store::open(&self.db).unwrap();
        spacetrace_hub::fleet::targets(&store).unwrap().len()
    }
}

async fn start_hub(keep: Option<usize>) -> Hub {
    start_hub_with(keep, "").await
}

/// Same, with extra TOML appended — for the settings the router reads at
/// startup rather than per request.
async fn start_hub_with(keep: Option<usize>, extra: &str) -> Hub {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("hub.sqlite");

    let mut toml = format!("db = {:?}\n", db.to_string_lossy());
    if let Some(keep) = keep {
        toml.push_str(&format!("keep_per_target = {keep}\n"));
    }
    toml.push_str(extra);
    let config: Config = toml::from_str(&toml).unwrap();

    // Both migrations, exactly as main.rs does before serving.
    let _ = Store::open(&db).unwrap();
    db::migrate(&Connection::open(&db).unwrap()).unwrap();

    let app = web::router(&config, ADMIN.to_string()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Hub {
        addr,
        db,
        _home: home,
    }
}

/// A real snapshot file, exactly what `spacetrace-agent push` would send.
fn snapshot_body(host: &str, root_name: &str, bytes: usize) -> Vec<u8> {
    let source = tempfile::tempdir().unwrap();
    let root = source.path().join(root_name);
    snapshot_body_at(host, &root, bytes)
}

/// Same, but scanning a caller-owned directory so repeated pushes describe the
/// *same* target. Retention and history are per (host, root), so a fresh
/// temporary path each time would look like a new machine every push.
fn snapshot_body_at(host: &str, root: &std::path::Path, bytes: usize) -> Vec<u8> {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("data.bin"), vec![0u8; bytes]).unwrap();

    let (tree, stats) = scan(
        root,
        ScanOptions::default(),
        Arc::new(ScanProgress::default()),
    )
    .unwrap();

    let work = tempfile::tempdir().unwrap();
    let db = work.path().join("sender.sqlite");
    let mut store = Store::open(&db).unwrap();
    let id = store.save(&tree, &stats, host, Some("nightly")).unwrap();

    let wire = work.path().join("wire.sqlite");
    store.export_snapshot(id, &wire).unwrap();
    std::fs::read(&wire).unwrap()
}

/// A client that does not follow redirects, so a redirect can be asserted on.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

// -------------------------------------------------------------------- auth

#[tokio::test]
async fn health_needs_no_credential() {
    let hub = start_hub(None).await;
    let response = client().get(hub.url("/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn the_dashboard_sends_a_browser_to_the_sign_in_page() {
    let hub = start_hub(None).await;
    for path in ["/", "/alerts", "/tokens", "/about"] {
        let response = client()
            .get(hub.url(path))
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303, "{path}");
        assert_eq!(response.headers()["location"], "/login", "{path}");
    }
}

#[tokio::test]
async fn the_api_answers_401_rather_than_redirecting() {
    let hub = start_hub(None).await;
    let response = client().get(hub.url("/api/fleet")).send().await.unwrap();
    assert_eq!(response.status(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
}

#[tokio::test]
async fn the_admin_token_opens_the_dashboard() {
    let hub = start_hub(None).await;
    let response = client()
        .get(hub.url("/api/fleet"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["summary"]["targets"], 0);
}

#[tokio::test]
async fn a_wrong_admin_token_is_refused() {
    let hub = start_hub(None).await;
    for wrong in ["", "nope", "admin-token-7c1e", "Admin-Token-7C1F"] {
        let response = client()
            .get(hub.url("/api/fleet"))
            .bearer_auth(wrong)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "token {wrong:?}");
    }
}

#[tokio::test]
async fn signing_in_sets_a_session_cookie_that_works() {
    let hub = start_hub(None).await;

    let response = client()
        .post(hub.url("/login"))
        .form(&[("token", ADMIN)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/");

    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("st_hub="), "{cookie}");
    // The session cookie must not be readable by script, and must not ride
    // along on requests from other sites.
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");

    let response = client()
        .get(hub.url("/"))
        .header("accept", "text/html")
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("Fleet"));
}

#[tokio::test]
async fn a_wrong_token_at_sign_in_does_not_set_a_cookie() {
    let hub = start_hub(None).await;
    let response = client()
        .post(hub.url("/login"))
        .form(&[("token", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "the form is shown again");
    assert!(!response.headers().contains_key("set-cookie"));
    assert!(response.text().await.unwrap().contains("not accepted"));
}

// ------------------------------------------------- the credential boundary

/// The point of two token types: a token sitting in a config file on a NAS
/// must not be a key to the whole fleet's inventory.
#[tokio::test]
async fn an_agent_token_cannot_read_the_dashboard() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    for path in ["/api/fleet", "/", "/tokens", "/alerts"] {
        let response = client()
            .get(hub.url(path))
            .bearer_auth(&agent)
            .send()
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            200,
            "{path} must not be readable with an agent token"
        );
    }
}

/// And the reverse: the admin token is not an agent credential, so it cannot
/// be used to inject snapshots.
#[tokio::test]
async fn the_admin_token_cannot_push_snapshots() {
    let hub = start_hub(None).await;
    let body = snapshot_body("nas", "var", 40_000);

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(ADMIN)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(hub.targets(), 0);
}

#[tokio::test]
async fn pushing_without_a_token_is_refused() {
    let hub = start_hub(None).await;
    let response = client()
        .post(hub.url("/snapshots"))
        .body(snapshot_body("nas", "var", 1000))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(hub.targets(), 0);
}

#[tokio::test]
async fn a_revoked_agent_token_stops_working() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    // It works first.
    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "var", 5000))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let id = db::list_tokens(&hub.conn()).unwrap()[0].id;
    db::revoke_token(&hub.conn(), id).unwrap();

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "srv", 5000))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(hub.targets(), 1, "the second push must not have landed");
}

// ------------------------------------------------------------------ ingest

#[tokio::test]
async fn a_pushed_snapshot_appears_on_the_dashboard() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "var", 120_000))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["imported"].as_array().unwrap().len(), 1);

    // The API sees it.
    let fleet: serde_json::Value = client()
        .get(hub.url("/api/fleet"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fleet["summary"]["targets"], 1);
    assert_eq!(fleet["targets"][0]["host"], "nas");

    // And so does the page, with the host actually rendered.
    let html = client()
        .get(hub.url("/"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("nas"), "the host should be listed");

    // Recording last_seen_at is what makes the Agents page useful.
    assert!(db::list_tokens(&hub.conn()).unwrap()[0]
        .last_seen_at
        .is_some());
}

#[tokio::test]
async fn a_zstd_compressed_push_is_accepted() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");
    let raw = snapshot_body("nas", "var", 200_000);
    let packed = zstd::encode_all(raw.as_slice(), 3).unwrap();
    assert!(packed.len() < raw.len());

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .header("content-encoding", "zstd")
        .body(packed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(hub.targets(), 1);
}

#[tokio::test]
async fn a_body_that_is_not_a_snapshot_is_a_client_error() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(vec![0u8; 4096])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(hub.targets(), 0);
}

#[tokio::test]
async fn a_body_claiming_to_be_zstd_but_is_not_is_a_client_error() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .header("content-encoding", "zstd")
        .body(b"not zstd at all".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn pushing_the_same_snapshot_twice_does_not_duplicate_it() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");
    let body = snapshot_body("nas", "var", 60_000);

    let first: serde_json::Value = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(body.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["imported"].as_array().unwrap().len(), 1);

    let second: serde_json::Value = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        second["imported"].as_array().unwrap().is_empty(),
        "a re-push must be a no-op"
    );
    assert_eq!(hub.targets(), 1);
}

#[tokio::test]
async fn retention_bounds_the_history_per_target() {
    let hub = start_hub(Some(2)).await;
    let agent = hub.new_agent_token("nas");

    // One directory, scanned repeatedly: that is what makes these three
    // snapshots of the *same* target rather than three different ones.
    let source = tempfile::tempdir().unwrap();
    let root = source.path().join("var");

    for round in 0..3 {
        if round > 0 {
            // started_at has one-second resolution and is part of the
            // duplicate key, so pushes inside one second are the same snapshot.
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }
        let response = client()
            .post(hub.url("/snapshots"))
            .bearer_auth(&agent)
            .body(snapshot_body_at("nas", &root, 10_000 + round * 1000))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }

    let store = Store::open(&hub.db).unwrap();
    let kept = store.list().unwrap();
    assert_eq!(
        kept.len(),
        2,
        "keep_per_target = 2 should bound it; kept {:?}",
        kept.iter().map(|m| m.id).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------------ alerts

/// Stand up a webhook receiver and hand back its URL plus what it collects.
async fn webhook_receiver() -> (String, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);

    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |body: axum::Json<serde_json::Value>| {
            let sink = Arc::clone(&sink);
            async move {
                sink.lock().unwrap().push(body.0);
                axum::http::StatusCode::NO_CONTENT
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/hook"), received)
}

/// A rule is evaluated on the push that makes it true, and actually delivered.
#[tokio::test]
async fn a_threshold_rule_fires_and_is_recorded() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");
    let (hook, received) = webhook_receiver().await;

    // "free space below 100%" is true of any real filesystem, so it fires on
    // the first push.
    db::create_rule(
        &hub.conn(),
        None,
        None,
        db::AlertKind::FreeBelowPercent,
        100.0,
        &hook,
        None,
    )
    .unwrap();

    let response: serde_json::Value = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "var", 30_000))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["alerts_fired"], 1, "the rule should have fired");

    let events = db::recent_events(&hub.conn(), 10).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].delivered, "detail: {:?}", events[0].detail);
    assert!(events[0].message.contains("nas"), "{}", events[0].message);

    // The webhook really received a usable payload.
    let payloads = received.lock().unwrap().clone();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["source"], "spacetrace-hub");
    assert_eq!(payloads[0]["host"], "nas");
    assert!(payloads[0]["root"].is_string());
    assert!(payloads[0]["message"].as_str().unwrap().contains("free"));

    // It shows up on the page.
    let html = client()
        .get(hub.url("/alerts"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("delivered"), "the event should be listed");
}

#[tokio::test]
async fn a_failed_webhook_is_recorded_as_failed_rather_than_lost() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    // Port 1 on loopback: nothing listens there.
    db::create_rule(
        &hub.conn(),
        None,
        None,
        db::AlertKind::FreeBelowPercent,
        100.0,
        "http://127.0.0.1:1/nope",
        None,
    )
    .unwrap();

    let response: serde_json::Value = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "var", 30_000))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The push itself must still succeed: a broken webhook is not the agent's
    // problem.
    assert_eq!(response["alerts_fired"], 0);
    assert_eq!(hub.targets(), 1, "the snapshot still landed");

    let events = db::recent_events(&hub.conn(), 10).unwrap();
    assert_eq!(events.len(), 1);
    assert!(!events[0].delivered);
    assert!(events[0].detail.is_some(), "the reason should be recorded");
}

#[tokio::test]
async fn nothing_fires_when_no_rule_is_configured() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    let response: serde_json::Value = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body("nas", "var", 30_000))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["alerts_fired"], 0);
    assert!(db::recent_events(&hub.conn(), 10).unwrap().is_empty());
}

// ------------------------------------------------------------------- pages

#[tokio::test]
async fn the_target_page_shows_a_history_and_survives_a_hostile_hostname() {
    let hub = start_hub(None).await;
    let agent = hub.new_agent_token("nas");

    // A hostname that would break out of an attribute if it were not escaped.
    let hostile = r#"nas"><script>alert(1)</script>"#;
    client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&agent)
        .body(snapshot_body(hostile, "var", 40_000))
        .send()
        .await
        .unwrap();

    let html = client()
        .get(hub.url("/"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        !html.contains("<script>alert(1)</script>"),
        "the hostname must be escaped, not rendered"
    );
    assert!(html.contains("&lt;script&gt;"), "it should appear escaped");
}

#[tokio::test]
async fn an_unknown_target_is_reported_rather_than_crashing() {
    let hub = start_hub(None).await;
    let response = client()
        .get(hub.url("/target?host=nowhere&root=%2Fnope"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("No snapshots"));
}

#[tokio::test]
async fn the_empty_dashboard_explains_how_to_get_started() {
    let hub = start_hub(None).await;
    let html = client()
        .get(hub.url("/"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("No snapshots yet"));
    assert!(html.contains("spacetrace-agent push"));
}

#[tokio::test]
async fn creating_an_agent_token_shows_it_exactly_once() {
    let hub = start_hub(None).await;

    let response = client()
        .post(hub.url("/tokens"))
        .header("cookie", format!("st_hub={ADMIN}"))
        .form(&[("name", "web1")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    let location = response.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with("/tokens?created="), "{location}");

    // The plaintext is in the redirect and shown on the page it points at...
    let html = client()
        .get(hub.url(&location))
        .header("cookie", format!("st_hub={ADMIN}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("Copy it now"));

    // ...but only its hash was stored.
    let stored: String = hub
        .conn()
        .query_row("SELECT token_sha256 FROM agent_tokens", [], |r| r.get(0))
        .unwrap();
    let plaintext = location.trim_start_matches("/tokens?created=");
    assert_ne!(stored, plaintext);
    assert_eq!(stored, db::hash_token(plaintext));

    // And it actually works as an agent token.
    let response = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(plaintext)
        .body(snapshot_body("web1", "srv", 20_000))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

// ------------------------------------------------------------- email alerts

/// Without `[smtp]`, the hub still runs and says plainly that it cannot send
/// mail. The failure this guards against is a settings page that renders
/// nothing, leaving someone to conclude the feature does not exist.
#[tokio::test]
async fn the_settings_page_says_when_no_mail_is_configured() {
    let hub = start_hub(None).await;
    let body = client()
        .get(hub.url("/settings"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("No mail server configured"), "{body}");
    assert!(body.contains("[smtp]"), "it must show what to add");
    assert!(
        !body.contains("Send a test message"),
        "there is nothing to test against"
    );
}

/// With `[smtp]`, the page describes the relay — and must not describe the
/// password. The whole reason credentials stay in the config file.
#[tokio::test]
async fn the_settings_page_describes_the_relay_without_its_password() {
    let hub = start_hub_with(
        None,
        r#"
[smtp]
host = "smtp.example.com"
port = 2525
security = "starttls"
from = "hub@example.com"
username = "hub@example.com"
password = "hunter2-should-never-appear"
"#,
    )
    .await;

    let body = client()
        .get(hub.url("/settings"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("smtp.example.com:2525"), "{body}");
    assert!(
        body.contains("hub@example.com"),
        "the From address is shown"
    );
    assert!(body.contains("Send a test message"));
    assert!(
        !body.contains("hunter2"),
        "the password must never reach the page"
    );
}

/// An email rule is accepted and listed as one. The form posts a bare address
/// and the dashboard shows the stored `mailto:` form, which is the round trip
/// a person actually sees.
#[tokio::test]
async fn an_alert_rule_can_send_email() {
    let hub = start_hub(None).await;

    let created = client()
        .post(hub.url("/alerts"))
        .bearer_auth(ADMIN)
        .form(&[
            ("host", ""),
            ("root", ""),
            ("kind", "free_below_percent"),
            ("threshold", "10"),
            ("destination", "ops@example.com"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 303, "a created rule redirects back");

    let body = client()
        .get(hub.url("/alerts"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("mailto:ops@example.com"), "{body}");
    // And with no relay configured it has to say the rule will not deliver,
    // rather than listing it as though it were fine.
    assert!(body.contains("No mail server is configured"), "{body}");
}

/// A destination that is neither must be refused at the form, not stored and
/// discovered at delivery time.
#[tokio::test]
async fn a_destination_that_is_neither_url_nor_address_is_refused() {
    let hub = start_hub(None).await;

    let response = client()
        .post(hub.url("/alerts"))
        .bearer_auth(ADMIN)
        .form(&[
            ("host", ""),
            ("root", ""),
            ("kind", "free_below_percent"),
            ("threshold", "10"),
            ("destination", "somewhere"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "the error page, not a redirect");
    assert_eq!(db::list_rules(&hub.conn()).unwrap().len(), 0);
}

// ------------------------------------------------------------ people, roles

/// Every route that changes something, as the test can reach it.
///
/// A list rather than a loop over the router, because axum does not expose its
/// routes — which means this list is the thing that rots. It is checked
/// against the source in `every_write_route_is_in_this_list` below, so adding
/// a POST route without adding it here fails rather than quietly leaving a
/// read-only account able to use it.
const WRITE_ROUTES: [(&str, &[(&str, &str)]); 7] = [
    (
        "/alerts",
        &[
            ("host", ""),
            ("root", ""),
            ("kind", "free_below_percent"),
            ("threshold", "10"),
            ("destination", "https://example.com/hook"),
        ],
    ),
    ("/alerts/delete", &[("id", "1")]),
    ("/tokens", &[("name", "sneaky")]),
    ("/tokens/revoke", &[("id", "1")]),
    ("/people", &[("name", "sneaky"), ("role", "admin")]),
    ("/people/revoke", &[("id", "1")]),
    ("/settings/test", &[("to", "ops@example.com")]),
];

/// The point of the feature: somebody can be given the dashboard without being
/// given the ability to change the fleet.
#[tokio::test]
async fn a_viewer_can_read_everything_and_change_nothing() {
    let hub = start_hub(None).await;
    let (_, viewer) = db::create_user(&hub.conn(), "reader", db::Role::Viewer).unwrap();

    for page in ["/", "/alerts", "/tokens", "/people", "/settings"] {
        let response = client()
            .get(hub.url(page))
            .bearer_auth(&viewer)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            200,
            "a viewer must be able to read {page}"
        );
    }

    for (route, form) in WRITE_ROUTES {
        let response = client()
            .post(hub.url(route))
            .bearer_auth(&viewer)
            .form(&form.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            403,
            "a viewer must not be able to POST {route}"
        );
    }

    // And nothing happened as a side effect of trying.
    assert_eq!(db::list_rules(&hub.conn()).unwrap().len(), 0);
    assert_eq!(db::list_tokens(&hub.conn()).unwrap().len(), 0);
    assert_eq!(
        db::list_users(&hub.conn()).unwrap().len(),
        1,
        "just the viewer"
    );
}

/// The list above is only a safety net while it is complete. This reads the
/// router in `src/web.rs` and fails when a `post(...)` route is not in it —
/// the failure mode being a new write route that every read-only account can
/// use, which no other test would notice.
#[test]
fn every_write_route_is_in_this_list() {
    let source = include_str!("../src/web.rs");
    let routes: Vec<String> = source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(".route(\"")?;
            let (path, rest) = rest.split_once('"')?;
            // `get(x).post(y)` counts too: a mixed route with a write on it
            // would sit in the read-only group.
            rest.contains("post(").then(|| path.to_string())
        })
        .collect();

    assert!(routes.len() > 3, "the router scan found almost nothing");
    for path in routes {
        // Signing in and out are not changes to the fleet.
        if path == "/login" || path == "/logout" || path == "/snapshots" {
            continue;
        }
        assert!(
            WRITE_ROUTES.iter().any(|(known, _)| *known == path),
            "{path} accepts POST but is not in WRITE_ROUTES — is it behind \
             require_write, and is a viewer refused?"
        );
    }
}

/// The same routes, from an admin, have to actually work — otherwise the test
/// above would pass on a hub where nobody can do anything.
#[tokio::test]
async fn an_admin_can_do_what_a_viewer_cannot() {
    let hub = start_hub(None).await;
    let (_, admin) = db::create_user(&hub.conn(), "owner", db::Role::Admin).unwrap();

    let response = client()
        .post(hub.url("/alerts"))
        .bearer_auth(&admin)
        .form(&[
            ("host", ""),
            ("root", ""),
            ("kind", "free_below_percent"),
            ("threshold", "10"),
            ("destination", "https://example.com/hook"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);

    let rules = db::list_rules(&hub.conn()).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].created_by.as_deref(),
        Some("owner"),
        "a rule has to carry who made it; that is the point of naming people"
    );
}

/// The credential in the config file keeps working and keeps being an admin.
/// It is the way back in, so a release that quietly demoted it would lock
/// people out of their own hub.
#[tokio::test]
async fn the_configured_token_still_works_and_is_an_admin() {
    let hub = start_hub(None).await;

    let response = client()
        .post(hub.url("/people"))
        .bearer_auth(ADMIN)
        .form(&[("name", "jane"), ("role", "viewer")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(db::list_users(&hub.conn()).unwrap().len(), 1);

    // And its actions are attributable to something rather than to nobody.
    client()
        .post(hub.url("/alerts"))
        .bearer_auth(ADMIN)
        .form(&[
            ("host", ""),
            ("root", ""),
            ("kind", "free_below_percent"),
            ("threshold", "10"),
            ("destination", "https://example.com/hook"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        db::list_rules(&hub.conn()).unwrap()[0]
            .created_by
            .as_deref(),
        Some("config")
    );
}

/// Taking access away has to actually take it away, on the next request.
#[tokio::test]
async fn a_revoked_person_is_locked_out() {
    let hub = start_hub(None).await;
    let (person, token) = db::create_user(&hub.conn(), "leaver", db::Role::Viewer).unwrap();

    assert_eq!(
        client()
            .get(hub.url("/"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    db::revoke_user(&hub.conn(), person.id).unwrap();

    let after = client()
        .get(hub.url("/"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), 401, "a revoked token must stop working");
}

/// Revoking the last admin would leave the dashboard changeable only by
/// editing a file on the server — which is exactly the position somebody is
/// not in when they have just locked themselves out of the web interface.
#[tokio::test]
async fn the_last_admin_cannot_revoke_themselves() {
    let hub = start_hub(None).await;
    let (only, token) = db::create_user(&hub.conn(), "solo", db::Role::Admin).unwrap();

    let response = client()
        .post(hub.url("/people/revoke"))
        .bearer_auth(&token)
        .form(&[("id", only.id.to_string())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "the refusal page, not a redirect");
    assert!(!db::list_users(&hub.conn()).unwrap()[0].revoked);

    // With a second admin there is a way back in, so it is allowed.
    db::create_user(&hub.conn(), "backup", db::Role::Admin).unwrap();
    let response = client()
        .post(hub.url("/people/revoke"))
        .bearer_auth(&token)
        .form(&[("id", only.id.to_string())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert!(
        db::list_users(&hub.conn())
            .unwrap()
            .iter()
            .find(|u| u.id == only.id)
            .unwrap()
            .revoked
    );
}

/// A person's token has to work through the sign-in form as well as the
/// Authorization header, or the only page that offers to take a token cannot
/// take theirs.
#[tokio::test]
async fn a_person_can_sign_in_through_the_form() {
    let hub = start_hub(None).await;
    let (_, token) = db::create_user(&hub.conn(), "jane", db::Role::Viewer).unwrap();

    let response = client()
        .post(hub.url("/login"))
        .form(&[("token", token.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("a session cookie")
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly"), "{cookie}");

    let page = client()
        .get(hub.url("/"))
        .header("cookie", format!("st_hub={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
}

/// An agent token opens nothing on the dashboard, and a dashboard token
/// pushes no snapshots. The separation predates roles and must survive them.
#[tokio::test]
async fn dashboard_tokens_and_agent_tokens_stay_separate() {
    let hub = start_hub(None).await;
    let (_, person) = db::create_user(&hub.conn(), "jane", db::Role::Admin).unwrap();
    let agent = hub.new_agent_token("nas");

    let as_agent = client()
        .get(hub.url("/"))
        .bearer_auth(&agent)
        .send()
        .await
        .unwrap();
    assert_eq!(as_agent.status(), 401, "an agent token is not a person");

    let as_person = client()
        .post(hub.url("/snapshots"))
        .bearer_auth(&person)
        .body(vec![0u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(as_person.status(), 401, "a person is not an agent");
}
