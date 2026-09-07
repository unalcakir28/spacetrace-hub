//! The hub's own tables, alongside the snapshot store.
//!
//! Snapshots themselves live in `spacetrace-store`, unchanged: a snapshot
//! pushed by an agent is imported as-is and the hub adds no format of its own.
//! What is here is the part a single agent has no need for — who is allowed to
//! push, and what should raise an alarm.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped when these tables change in a way an older binary cannot read.
/// Kept separate from the snapshot schema's version, which the store owns.
const HUB_SCHEMA_VERSION: i64 = 1;

pub fn migrate(conn: &Connection) -> Result<()> {
    // On a fresh database `hub_meta` does not exist yet and the query fails;
    // that is version 0, not an error.
    let found: i64 = conn
        .query_row(
            "SELECT value FROM hub_meta WHERE key = 'schema'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    anyhow::ensure!(
        found <= HUB_SCHEMA_VERSION,
        "this database was written by a newer spacetrace-hub (hub schema v{found}, \
         this build understands v{HUB_SCHEMA_VERSION})"
    );

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS hub_meta (
            key   TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        );

        -- Only the hash is stored. A leaked database should not hand over
        -- working credentials for every agent in the fleet.
        CREATE TABLE IF NOT EXISTS agent_tokens (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            name         TEXT    NOT NULL,
            token_sha256 TEXT    NOT NULL UNIQUE,
            created_at   INTEGER NOT NULL,
            last_seen_at INTEGER,
            revoked      INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS alert_rules (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            -- NULL matches any host / any root.
            host        TEXT,
            root        TEXT,
            kind        TEXT    NOT NULL,
            threshold   REAL    NOT NULL,
            webhook_url TEXT    NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            created_at  INTEGER NOT NULL
        );

        -- What actually fired, so a rule cannot spam and so there is a record
        -- to look at after the fact.
        CREATE TABLE IF NOT EXISTS alert_events (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id  INTEGER NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
            host     TEXT    NOT NULL,
            root     TEXT    NOT NULL,
            fired_at INTEGER NOT NULL,
            message  TEXT    NOT NULL,
            delivered INTEGER NOT NULL DEFAULT 0,
            detail   TEXT
        );

        CREATE INDEX IF NOT EXISTS alert_events_recent
            ON alert_events (fired_at DESC);
        CREATE INDEX IF NOT EXISTS alert_events_target
            ON alert_events (rule_id, host, root, fired_at DESC);
        "#,
    )?;

    conn.execute(
        "INSERT INTO hub_meta (key, value) VALUES ('schema', ?1)
         ON CONFLICT(key) DO UPDATE SET value = ?1",
        [HUB_SCHEMA_VERSION],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ tokens

#[derive(Debug, Clone, Serialize)]
pub struct AgentToken {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked: bool,
}

/// SHA-256 of the token as lowercase hex.
///
/// A plain hash rather than a password KDF on purpose: these are 256 bits of
/// machine-generated randomness, so there is no dictionary to attack and
/// nothing for a slow hash to buy. It only has to stop a stolen database from
/// being a set of working credentials.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A fresh 256-bit token, hex encoded.
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("reading randomness from the OS: {e}"))?;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

/// Create a token. The plaintext is returned once and never stored.
pub fn create_token(conn: &Connection, name: &str) -> Result<(AgentToken, String)> {
    anyhow::ensure!(!name.trim().is_empty(), "a token needs a name");
    let plaintext = generate_token()?;
    let now = now_unix();
    conn.execute(
        "INSERT INTO agent_tokens (name, token_sha256, created_at) VALUES (?1, ?2, ?3)",
        params![name.trim(), hash_token(&plaintext), now],
    )?;
    let id = conn.last_insert_rowid();
    Ok((
        AgentToken {
            id,
            name: name.trim().to_string(),
            created_at: now,
            last_seen_at: None,
            revoked: false,
        },
        plaintext,
    ))
}

/// Look up a presented token. Returns `None` for unknown or revoked tokens.
///
/// The lookup is by hash, so the comparison happens inside SQLite on a unique
/// index rather than by walking every row in Rust.
pub fn verify_token(conn: &Connection, presented: &str) -> Result<Option<AgentToken>> {
    if presented.trim().is_empty() {
        return Ok(None);
    }
    let hash = hash_token(presented.trim());
    let found = conn
        .query_row(
            "SELECT id, name, created_at, last_seen_at, revoked
             FROM agent_tokens WHERE token_sha256 = ?1",
            [&hash],
            |row| {
                Ok(AgentToken {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    last_seen_at: row.get(3)?,
                    revoked: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;

    match found {
        Some(token) if !token.revoked => {
            conn.execute(
                "UPDATE agent_tokens SET last_seen_at = ?1 WHERE id = ?2",
                params![now_unix(), token.id],
            )?;
            Ok(Some(token))
        }
        _ => Ok(None),
    }
}

pub fn list_tokens(conn: &Connection) -> Result<Vec<AgentToken>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at, last_seen_at, revoked
         FROM agent_tokens ORDER BY revoked, created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(AgentToken {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            last_seen_at: row.get(3)?,
            revoked: row.get::<_, i64>(4)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Revoke rather than delete, so the audit trail survives.
pub fn revoke_token(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute("UPDATE agent_tokens SET revoked = 1 WHERE id = ?1", [id])?;
    Ok(n > 0)
}

// ------------------------------------------------------------------ alerts

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// Free space on the target's filesystem fell below `threshold` percent.
    FreeBelowPercent,
    /// The target grew by more than `threshold` bytes per day.
    GrowthAbovePerDay,
    /// The forecast says the filesystem fills within `threshold` days.
    FullWithinDays,
}

impl AlertKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertKind::FreeBelowPercent => "free_below_percent",
            AlertKind::GrowthAbovePerDay => "growth_above_per_day",
            AlertKind::FullWithinDays => "full_within_days",
        }
    }

    pub fn parse(text: &str) -> Option<AlertKind> {
        match text {
            "free_below_percent" => Some(AlertKind::FreeBelowPercent),
            "growth_above_per_day" => Some(AlertKind::GrowthAbovePerDay),
            "full_within_days" => Some(AlertKind::FullWithinDays),
            _ => None,
        }
    }

    pub fn describe(&self, threshold: f64) -> String {
        match self {
            AlertKind::FreeBelowPercent => format!("free space below {threshold:.0}%"),
            AlertKind::GrowthAbovePerDay => {
                format!(
                    "growing faster than {}/day",
                    crate::html::bytes(threshold as u64)
                )
            }
            AlertKind::FullWithinDays => format!("full within {threshold:.0} days"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertRule {
    pub id: i64,
    /// `None` matches any host.
    pub host: Option<String>,
    /// `None` matches any root.
    pub root: Option<String>,
    pub kind: AlertKind,
    pub threshold: f64,
    pub webhook_url: String,
    pub enabled: bool,
    pub created_at: i64,
}

impl AlertRule {
    /// Whether this rule is about the given target.
    pub fn matches(&self, host: &str, root: &str) -> bool {
        self.host.as_deref().is_none_or(|h| h == host)
            && self.root.as_deref().is_none_or(|r| r == root)
    }
}

pub fn create_rule(
    conn: &Connection,
    host: Option<&str>,
    root: Option<&str>,
    kind: AlertKind,
    threshold: f64,
    webhook_url: &str,
) -> Result<i64> {
    anyhow::ensure!(
        webhook_url.starts_with("http://") || webhook_url.starts_with("https://"),
        "the webhook URL must be http or https"
    );
    anyhow::ensure!(
        threshold.is_finite() && threshold >= 0.0,
        "the threshold must be a non-negative number"
    );
    conn.execute(
        "INSERT INTO alert_rules (host, root, kind, threshold, webhook_url, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            host.map(str::trim).filter(|s| !s.is_empty()),
            root.map(str::trim).filter(|s| !s.is_empty()),
            kind.as_str(),
            threshold,
            webhook_url.trim(),
            now_unix()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_rules(conn: &Connection) -> Result<Vec<AlertRule>> {
    let mut stmt = conn.prepare(
        "SELECT id, host, root, kind, threshold, webhook_url, enabled, created_at
         FROM alert_rules ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let kind_text: String = row.get(3)?;
        Ok(AlertRule {
            id: row.get(0)?,
            host: row.get(1)?,
            root: row.get(2)?,
            // An unrecognised kind means the row was written by a newer build.
            // Treat it as the least alarming option rather than failing the
            // whole listing.
            kind: AlertKind::parse(&kind_text).unwrap_or(AlertKind::FreeBelowPercent),
            threshold: row.get(4)?,
            webhook_url: row.get(5)?,
            enabled: row.get::<_, i64>(6)? != 0,
            created_at: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_rule(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute("DELETE FROM alert_rules WHERE id = ?1", [id])?;
    Ok(n > 0)
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertEvent {
    pub id: i64,
    pub rule_id: i64,
    pub host: String,
    pub root: String,
    pub fired_at: i64,
    pub message: String,
    pub delivered: bool,
    pub detail: Option<String>,
}

/// Record that a rule fired.
///
/// `fired_at` is passed in rather than read from the clock here: the caller
/// compares it against [`last_fired`] to apply the cooldown, and the two must
/// be on the same clock or the cooldown is computed against a different
/// timeline than it is recorded on.
pub fn record_event(
    conn: &Connection,
    rule_id: i64,
    host: &str,
    root: &str,
    fired_at: i64,
    message: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO alert_events (rule_id, host, root, fired_at, message)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![rule_id, host, root, fired_at, message],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn mark_delivered(conn: &Connection, event_id: i64, detail: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE alert_events SET delivered = 1, detail = ?2 WHERE id = ?1",
        params![event_id, detail],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, event_id: i64, detail: &str) -> Result<()> {
    conn.execute(
        "UPDATE alert_events SET delivered = 0, detail = ?2 WHERE id = ?1",
        params![event_id, detail],
    )?;
    Ok(())
}

/// When this rule last fired for this target, so it can be held back.
pub fn last_fired(conn: &Connection, rule_id: i64, host: &str, root: &str) -> Result<Option<i64>> {
    let found = conn
        .query_row(
            "SELECT MAX(fired_at) FROM alert_events
             WHERE rule_id = ?1 AND host = ?2 AND root = ?3",
            params![rule_id, host, root],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?;
    Ok(found.flatten())
}

pub fn recent_events(conn: &Connection, limit: usize) -> Result<Vec<AlertEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, rule_id, host, root, fired_at, message, delivered, detail
         FROM alert_events ORDER BY fired_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(AlertEvent {
            id: row.get(0)?,
            rule_id: row.get(1)?,
            host: row.get(2)?,
            root: row.get(3)?,
            fired_at: row.get(4)?,
            message: row.get(5)?,
            delivered: row.get::<_, i64>(6)? != 0,
            detail: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migrating_twice_is_harmless() {
        let conn = db();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert!(list_tokens(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_future_hub_schema_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute("UPDATE hub_meta SET value = 99 WHERE key = 'schema'", [])
            .unwrap();
        let err = migrate(&conn).unwrap_err().to_string();
        assert!(err.contains("newer spacetrace-hub"), "{err}");
    }

    #[test]
    fn generated_tokens_are_long_and_distinct() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64, "256 bits as hex");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_token_verifies_once_created_and_the_plaintext_is_not_stored() {
        let conn = db();
        let (token, plaintext) = create_token(&conn, "nas").unwrap();

        let found = verify_token(&conn, &plaintext).unwrap().unwrap();
        assert_eq!(found.id, token.id);
        assert_eq!(found.name, "nas");

        // The plaintext must not be recoverable from the database.
        let stored: String = conn
            .query_row(
                "SELECT token_sha256 FROM agent_tokens WHERE id = ?1",
                [token.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(stored, plaintext);
        assert_eq!(stored, hash_token(&plaintext));
    }

    #[test]
    fn verifying_records_when_the_agent_was_last_seen() {
        let conn = db();
        let (token, plaintext) = create_token(&conn, "nas").unwrap();
        assert!(token.last_seen_at.is_none());

        verify_token(&conn, &plaintext).unwrap().unwrap();
        let listed = list_tokens(&conn).unwrap();
        assert!(listed[0].last_seen_at.is_some());
    }

    #[test]
    fn unknown_and_revoked_tokens_are_refused() {
        let conn = db();
        let (token, plaintext) = create_token(&conn, "nas").unwrap();

        assert!(verify_token(&conn, "nope").unwrap().is_none());
        assert!(verify_token(&conn, "").unwrap().is_none());
        assert!(verify_token(&conn, "   ").unwrap().is_none());

        assert!(revoke_token(&conn, token.id).unwrap());
        assert!(
            verify_token(&conn, &plaintext).unwrap().is_none(),
            "a revoked token must stop working"
        );
        // Revoking keeps the row, so the record of it survives.
        assert_eq!(list_tokens(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_token_is_matched_after_surrounding_whitespace_is_trimmed() {
        let conn = db();
        let (_, plaintext) = create_token(&conn, "nas").unwrap();
        assert!(verify_token(&conn, &format!("  {plaintext}  "))
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_nameless_token_is_refused() {
        let conn = db();
        assert!(create_token(&conn, "").is_err());
        assert!(create_token(&conn, "   ").is_err());
    }

    #[test]
    fn rules_match_on_host_and_root_with_null_as_wildcard() {
        let conn = db();
        create_rule(
            &conn,
            Some("nas"),
            Some("/var"),
            AlertKind::FreeBelowPercent,
            10.0,
            "https://example.com/hook",
        )
        .unwrap();
        create_rule(
            &conn,
            None,
            None,
            AlertKind::FullWithinDays,
            7.0,
            "https://example.com/any",
        )
        .unwrap();
        create_rule(
            &conn,
            Some("web1"),
            None,
            AlertKind::GrowthAbovePerDay,
            1e9,
            "https://example.com/web",
        )
        .unwrap();

        let rules = list_rules(&conn).unwrap();
        assert_eq!(rules.len(), 3);

        let for_nas_var: Vec<_> = rules.iter().filter(|r| r.matches("nas", "/var")).collect();
        assert_eq!(for_nas_var.len(), 2, "the specific rule and the catch-all");

        let for_web1_srv: Vec<_> = rules.iter().filter(|r| r.matches("web1", "/srv")).collect();
        assert_eq!(for_web1_srv.len(), 2, "the host rule and the catch-all");

        let for_other: Vec<_> = rules.iter().filter(|r| r.matches("db1", "/data")).collect();
        assert_eq!(for_other.len(), 1, "only the catch-all");
    }

    #[test]
    fn a_rule_needs_a_real_webhook_and_a_sane_threshold() {
        let conn = db();
        assert!(create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            10.0,
            "not-a-url"
        )
        .is_err());
        assert!(create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            10.0,
            "ftp://x/y"
        )
        .is_err());
        assert!(create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            -1.0,
            "https://x/y"
        )
        .is_err());
        assert!(create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            f64::NAN,
            "https://x/y"
        )
        .is_err());
    }

    #[test]
    fn empty_host_and_root_strings_become_wildcards() {
        let conn = db();
        create_rule(
            &conn,
            Some("  "),
            Some(""),
            AlertKind::FreeBelowPercent,
            5.0,
            "https://x/y",
        )
        .unwrap();
        let rule = &list_rules(&conn).unwrap()[0];
        assert!(rule.host.is_none());
        assert!(rule.root.is_none());
        assert!(rule.matches("anything", "/anywhere"));
    }

    #[test]
    fn deleting_a_rule_removes_it_and_its_events() {
        let conn = db();
        let id = create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            5.0,
            "https://x/y",
        )
        .unwrap();
        record_event(&conn, id, "nas", "/var", now_unix(), "test").unwrap();
        assert_eq!(recent_events(&conn, 10).unwrap().len(), 1);

        assert!(delete_rule(&conn, id).unwrap());
        assert!(!delete_rule(&conn, id).unwrap());
        assert!(
            recent_events(&conn, 10).unwrap().is_empty(),
            "events cascade with their rule"
        );
    }

    #[test]
    fn events_track_delivery_and_the_last_time_a_rule_fired() {
        let conn = db();
        let rule = create_rule(
            &conn,
            None,
            None,
            AlertKind::FullWithinDays,
            7.0,
            "https://x/y",
        )
        .unwrap();
        assert!(last_fired(&conn, rule, "nas", "/var").unwrap().is_none());

        let event =
            record_event(&conn, rule, "nas", "/var", now_unix(), "fills in 3 days").unwrap();
        assert!(last_fired(&conn, rule, "nas", "/var").unwrap().is_some());
        // A different target has its own history.
        assert!(last_fired(&conn, rule, "nas", "/srv").unwrap().is_none());

        let listed = recent_events(&conn, 10).unwrap();
        assert!(!listed[0].delivered);

        mark_delivered(&conn, event, Some("204")).unwrap();
        let listed = recent_events(&conn, 10).unwrap();
        assert!(listed[0].delivered);
        assert_eq!(listed[0].detail.as_deref(), Some("204"));

        mark_failed(&conn, event, "connection refused").unwrap();
        let listed = recent_events(&conn, 10).unwrap();
        assert!(!listed[0].delivered);
        assert_eq!(listed[0].detail.as_deref(), Some("connection refused"));
    }

    #[test]
    fn alert_kinds_round_trip_through_their_text_form() {
        for kind in [
            AlertKind::FreeBelowPercent,
            AlertKind::GrowthAbovePerDay,
            AlertKind::FullWithinDays,
        ] {
            assert_eq!(AlertKind::parse(kind.as_str()), Some(kind));
            assert!(!kind.describe(10.0).is_empty());
        }
        assert!(AlertKind::parse("something_new").is_none());
    }
}
