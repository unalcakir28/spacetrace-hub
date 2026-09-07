//! Server-rendered HTML, by hand.
//!
//! No template engine and no frontend build: the hub is meant to be one static
//! binary you drop on a box, and a self-hosted tool that needs `npm install`
//! before it will show you a page is a worse tool. The pages here are small
//! enough that `format!` is honest about what it costs.
//!
//! Everything that reaches a page goes through [`escape`]. Hostnames, paths and
//! labels all come from agents, which means they come from whoever runs an
//! agent — untrusted as far as this process is concerned.

use std::fmt::Write as _;

/// Escape text for use in HTML element content or a quoted attribute.
///
/// The single and double quote cases are what make it safe inside
/// `attr="..."`; without them a path containing a quote could close the
/// attribute and add another.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Binary units with one decimal, matching the CLI and the desktop app.
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut n = value as f64;
    let mut unit = 0;
    while n >= 1024.0 && unit < UNITS.len() - 1 {
        n /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value, UNITS[0])
    } else {
        format!("{n:.1} {}", UNITS[unit])
    }
}

/// Signed, for a column of changes.
pub fn delta(value: i64) -> String {
    format!(
        "{}{}",
        if value < 0 { "−" } else { "+" },
        bytes(value.unsigned_abs())
    )
}

pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// `YYYY-MM-DD HH:MM` in UTC. Same hand-rolled civil-date maths as the rest of
/// the project, for the same reason: no timezone database in the binary.
pub fn timestamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60
    )
}

pub fn relative(unix: i64, now: i64) -> String {
    let seconds = now - unix;
    if seconds < 0 {
        return "in the future".into();
    }
    if seconds < 90 {
        return "just now".into();
    }
    let minutes = seconds / 60;
    if minutes < 90 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 36 {
        return format!("{hours} h ago");
    }
    let days = hours / 24;
    if days < 45 {
        return format!("{days} d ago");
    }
    format!("{} mo ago", days / 30)
}

/// Growth rate as bytes per day, or a dash when there is no usable trend.
pub fn rate(bytes_per_day: Option<f64>) -> String {
    match bytes_per_day {
        Some(rate) if rate.abs() < 1024.0 => "≈0/day".into(),
        Some(rate) if rate > 0.0 => format!("+{}/day", bytes(rate as u64)),
        Some(rate) => format!("−{}/day", bytes((-rate) as u64)),
        None => "—".into(),
    }
}

/// "3 days" / "5 months", or a dash. Rounded honestly: a forecast is not a
/// timestamp, so it is never shown to the hour.
pub fn horizon(days: Option<f64>) -> String {
    match days {
        None => "—".into(),
        Some(d) if d < 1.0 => "under a day".into(),
        Some(d) if d < 45.0 => format!("{:.0} days", d),
        Some(d) if d < 730.0 => format!("{:.0} months", d / 30.0),
        Some(d) => format!("{:.0} years", d / 365.0),
    }
}

/// Wrap body content in the shared page chrome.
pub fn page(title: &str, active: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + STYLE.len() + 1024);
    let _ = write!(
        out,
        r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · spacetrace hub</title>
<style>{STYLE}</style>
</head><body>
<header>
  <span class="brand">spacetrace <em>hub</em></span>
  <nav>{nav}</nav>
</header>
<main>{body}</main>
</body></html>"#,
        title = escape(title),
        STYLE = STYLE,
        nav = nav(active),
        body = body,
    );
    out
}

fn nav(active: &str) -> String {
    const LINKS: [(&str, &str); 4] = [
        ("/", "Fleet"),
        ("/alerts", "Alerts"),
        ("/tokens", "Agents"),
        ("/about", "About"),
    ];
    LINKS
        .iter()
        .map(|(href, label)| {
            let class = if *href == active { " class=\"on\"" } else { "" };
            format!("<a href=\"{href}\"{class}>{label}</a>")
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Shared stylesheet. Same visual language as the desktop app: dark, dense,
/// numbers in a monospace column.
const STYLE: &str = r#"
:root{--bg:#0b0f14;--panel:#111820;--border:#1e2933;--border2:#2b3947;
--text:#dbe4ec;--dim:#93a3b3;--faint:#64748b;--accent:#38bdaf;
--grow:#f87171;--shrink:#4ade80;--warn:#fbbf24;
--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
--sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,system-ui,sans-serif;
color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 var(--sans)}
header{display:flex;align-items:center;gap:20px;padding:11px 20px;
background:var(--panel);border-bottom:1px solid var(--border)}
.brand{font-weight:600;letter-spacing:-0.01em}
.brand em{color:var(--accent);font-style:normal}
nav{display:flex;gap:3px}
nav a{color:var(--dim);text-decoration:none;padding:5px 11px;border-radius:5px;font-size:13px}
nav a:hover{background:#16202a;color:var(--text)}
nav a.on{background:rgba(56,189,175,.13);color:var(--accent)}
main{max-width:1180px;margin:0 auto;padding:22px 20px 60px}
h1{font-size:19px;margin:0 0 4px;letter-spacing:-0.02em}
h2{font-size:15px;margin:28px 0 10px}
.lede{color:var(--dim);margin:0 0 20px;font-size:13px}
table{width:100%;border-collapse:collapse;margin-bottom:8px}
th{text-align:left;font-size:10.5px;text-transform:uppercase;letter-spacing:.06em;
color:var(--faint);font-weight:600;padding:6px 9px;border-bottom:1px solid var(--border2)}
td{padding:7px 9px;border-bottom:1px solid #17202a;font-family:var(--mono);font-size:12px}
tbody tr:hover{background:#141d26}
.r{text-align:right}
.num{font-variant-numeric:tabular-nums}
a{color:var(--accent)}
.card{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:15px;margin-bottom:14px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(210px,1fr));gap:12px;margin-bottom:20px}
.stat{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:13px 15px}
.stat .k{font-size:10.5px;text-transform:uppercase;letter-spacing:.06em;color:var(--faint);font-weight:600}
.stat .v{font-family:var(--mono);font-size:21px;margin-top:3px;font-variant-numeric:tabular-nums}
.stat .s{font-size:11.5px;color:var(--dim);margin-top:2px}
.bar{height:5px;border-radius:3px;background:#1d2733;overflow:hidden;margin-top:7px}
.bar span{display:block;height:100%;background:var(--accent)}
.bar.hot span{background:var(--grow)}
.bar.warm span{background:var(--warn)}
.tag{display:inline-block;padding:1px 7px;border-radius:3px;font-size:11px;
border:1px solid var(--border2);color:var(--dim);font-family:var(--sans)}
.tag.ok{color:#a7f3ec;border-color:#1f6f68;background:rgba(56,189,175,.1)}
.tag.warn{color:#fde68a;border-color:#7c5e12;background:rgba(251,191,36,.1)}
.tag.bad{color:#fecaca;border-color:#7f2d2d;background:rgba(248,113,113,.1)}
.up{color:var(--grow)}.down{color:var(--shrink)}
.empty{color:var(--faint);padding:24px 10px;text-align:center;font-size:13px}
form{display:grid;gap:10px}
label{font-size:11.5px;color:var(--dim);display:grid;gap:4px}
input,select{font:inherit;color:var(--text);background:#0c1218;
border:1px solid var(--border2);border-radius:5px;padding:6px 9px}
input:focus,select:focus{outline:2px solid var(--accent);outline-offset:1px}
button{font:inherit;color:#eafffb;background:#1f6f68;border:1px solid var(--accent);
border-radius:5px;padding:6px 13px;cursor:pointer}
button:hover{background:#26837a}
button.danger{background:transparent;border-color:#7f2d2d;color:#fca5a5}
button.danger:hover{background:rgba(248,113,113,.1)}
.row{display:flex;gap:9px;align-items:end;flex-wrap:wrap}
.row>label{flex:1;min-width:150px}
code{font-family:var(--mono);font-size:12px;background:#0c1218;padding:1px 5px;border-radius:3px}
pre{font-family:var(--mono);font-size:12px;background:#0c1218;border:1px solid var(--border);
border-radius:6px;padding:11px;overflow-x:auto}
.token{font-family:var(--mono);font-size:12.5px;word-break:break-all;
background:rgba(56,189,175,.09);border:1px solid #1f6f68;border-radius:6px;padding:10px}
.hint{font-size:11.5px;color:var(--faint);line-height:1.55}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_neutralises_markup_and_quotes() {
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("a & b"), "a &amp; b");
        assert_eq!(escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(escape("it's"), "it&#39;s");
        assert_eq!(escape("plain/path-1.txt"), "plain/path-1.txt");
    }

    /// Hostnames and paths come from agents, so a page must survive one that is
    /// trying to inject markup.
    #[test]
    fn a_hostile_hostname_cannot_break_out_of_an_attribute() {
        let hostile = r#"" onmouseover="alert(1)" x=""#;
        let escaped = escape(hostile);
        assert!(!escaped.contains('"'), "{escaped}");
        assert!(!escaped.contains("onmouseover=\""));
    }

    #[test]
    fn a_hostile_path_is_inert_in_element_content() {
        let escaped = escape("/var/<img src=x onerror=alert(1)>");
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
    }

    #[test]
    fn bytes_use_binary_units_like_the_rest_of_the_project() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn deltas_always_carry_a_sign() {
        assert_eq!(delta(0), "+0 B");
        assert_eq!(delta(2048), "+2.0 KiB");
        assert_eq!(delta(-2048), "−2.0 KiB");
        // i64::MIN must not panic on negation.
        assert!(delta(i64::MIN).starts_with('−'));
    }

    #[test]
    fn counts_are_grouped() {
        assert_eq!(count(7), "7");
        assert_eq!(count(1234), "1 234");
        assert_eq!(count(1_234_567), "1 234 567");
    }

    #[test]
    fn timestamps_render_known_instants() {
        assert_eq!(timestamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(timestamp(1_788_714_000), "2026-09-06 17:00 UTC");
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = 1_000_000i64;
        assert_eq!(relative(now, now), "just now");
        assert_eq!(relative(now - 600, now), "10 min ago");
        assert_eq!(relative(now - 7200, now), "2 h ago");
        assert_eq!(relative(now - 86_400 * 3, now), "3 d ago");
        assert_eq!(relative(now - 86_400 * 90, now), "3 mo ago");
        // A clock skew between agent and hub should not produce nonsense.
        assert_eq!(relative(now + 500, now), "in the future");
    }

    #[test]
    fn rates_distinguish_growth_shrinkage_and_noise() {
        assert_eq!(rate(None), "—");
        assert_eq!(rate(Some(0.0)), "≈0/day");
        assert_eq!(rate(Some(500.0)), "≈0/day", "below a KiB is noise");
        assert!(rate(Some(5.0 * 1024.0 * 1024.0)).starts_with('+'));
        assert!(rate(Some(-5.0 * 1024.0 * 1024.0)).starts_with('−'));
    }

    #[test]
    fn a_forecast_is_never_shown_more_precisely_than_it_deserves() {
        assert_eq!(horizon(None), "—");
        assert_eq!(horizon(Some(0.4)), "under a day");
        assert_eq!(horizon(Some(3.2)), "3 days");
        assert_eq!(horizon(Some(90.0)), "3 months");
        assert_eq!(horizon(Some(1095.0)), "3 years");
    }

    #[test]
    fn the_page_shell_marks_the_active_nav_entry_and_escapes_the_title() {
        let html = page("<b>Fleet</b>", "/alerts", "<p>hi</p>");
        assert!(
            html.contains("&lt;b&gt;Fleet&lt;/b&gt;"),
            "title must be escaped"
        );
        assert!(html.contains(r#"<a href="/alerts" class="on">Alerts</a>"#));
        assert!(html.contains(r#"<a href="/">Fleet</a>"#));
        assert!(
            html.contains("<p>hi</p>"),
            "body is passed through as markup"
        );
        assert!(html.starts_with("<!doctype html>"));
    }
}
