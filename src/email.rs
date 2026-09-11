//! Sending an alert as email.
//!
//! **Why a dependency at all.** This project writes small things by hand — the
//! cron parser and the calendar arithmetic are both here rather than pulled in
//! — and SMTP looks like it belongs on that list. It does not. Doing it by
//! hand means TLS (there is none in `std`), STARTTLS negotiation, SASL, and
//! then the part that actually bites: a message body and subject built from a
//! hostname and a path that arrived over the network. A bare CRLF in either
//! ends the header block and lets the rest be read as headers — an extra
//! `Bcc:` on an alert nobody reads carefully. `lettre` refuses those at the
//! type level, which is the whole reason it is here.
//!
//! Pulled in with rustls only: the hub ships as a static musl binary in a
//! container, and an OpenSSL dependency would end that. Checked rather than
//! assumed — `cargo tree` finds no `openssl` or `native-tls` in the graph.
//!
//! **Credentials are not stored in the database.** They come from the config
//! file, with a `password_file` option beside the inline one, exactly as the
//! admin token already works. A hub database is copied around — it is the
//! thing you back up — and a mail password inside it travels with every copy.

use anyhow::{Context, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{SmtpConfig, SmtpSecurity};

/// A configured sender, built once at startup.
///
/// Built once rather than per message because building it resolves the TLS
/// root store, and because a configuration error should stop the hub at
/// startup where someone is watching, not at 3 a.m. when the first alert
/// fires and silently fails to go out.
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: lettre::message::Mailbox,
    /// Kept for the settings page, which says what is configured without ever
    /// saying the password.
    pub describe: Described,
}

/// What the settings page is allowed to show.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Described {
    pub host: String,
    pub port: u16,
    pub security: &'static str,
    pub from: String,
    /// Whether a username was configured. Never the username's password, and
    /// never the password's length — that is a hint nobody needs.
    pub authenticated: bool,
}

impl Mailer {
    pub fn build(config: &SmtpConfig) -> Result<Self> {
        let from: lettre::message::Mailbox = config
            .from
            .parse()
            .with_context(|| format!("smtp.from is not an email address: {}", config.from))?;

        let mut builder = match config.security {
            // Implicit TLS from the first byte, the modern default (port 465).
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .with_context(|| format!("cannot reach SMTP relay {}", config.host))?,
            // Plaintext connection upgraded by STARTTLS (port 587).
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                    .with_context(|| format!("cannot reach SMTP relay {}", config.host))?
            }
            // No encryption at all. Only sane for a relay on localhost or a
            // trusted LAN, and the config validator says so.
            SmtpSecurity::None => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host).tls(Tls::None)
            }
        };

        builder = builder.port(config.port);

        if config.security != SmtpSecurity::None && config.accept_invalid_certs {
            // Deliberately awkward to reach and named for what it does. An
            // internal relay with a self-signed certificate is a real
            // situation; silently accepting any certificate is not, so this
            // only applies when someone wrote it down.
            let params = TlsParameters::builder(config.host.clone())
                .dangerous_accept_invalid_certs(true)
                .dangerous_accept_invalid_hostnames(true)
                .build()
                .context("building relaxed TLS parameters")?;
            builder = builder.tls(match config.security {
                SmtpSecurity::Starttls => Tls::Required(params),
                _ => Tls::Wrapper(params),
            });
        }

        if let Some(username) = config.username.as_deref() {
            let password = config.password()?.unwrap_or_default();
            builder = builder.credentials(Credentials::new(username.to_string(), password));
        }

        builder = builder.timeout(Some(std::time::Duration::from_secs(config.timeout_seconds)));

        Ok(Mailer {
            describe: Described {
                host: config.host.clone(),
                port: config.port,
                security: config.security.as_str(),
                from: config.from.clone(),
                authenticated: config.username.is_some(),
            },
            transport: builder.build(),
            from,
        })
    }

    /// Send one alert.
    ///
    /// Plain text, not HTML. An alert is read on a phone at an awkward hour
    /// and forwarded into a ticket; every step of that treats plain text
    /// better, and there is nothing here that formatting would clarify.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<String, String> {
        let recipient: lettre::message::Mailbox = to
            .parse()
            .map_err(|e| format!("{to} is not an email address: {e}"))?;

        let message = Message::builder()
            .from(self.from.clone())
            .to(recipient)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| format!("building the message: {e}"))?;

        let response = self
            .transport
            .send(message)
            .await
            .map_err(|e| format!("{e}"))?;

        // The relay's own reply, kept as the delivery detail so the events
        // table records what the server said rather than "ok". "Accepted for
        // delivery" is as far as SMTP goes: what happens after the relay takes
        // it is not something this hub can see, and the events page must not
        // imply otherwise.
        Ok(response
            .message()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string())
    }
}

/// The subject line for an alert.
///
/// The host and root go in it because the first thing anyone does with an
/// alert mail is decide whether it is theirs, and that decision is made from
/// the subject in a list. Built here rather than inline so the one place that
/// composes untrusted text into a header is easy to find.
pub fn subject_for(host: &str, root: &str) -> String {
    format!("spacetrace: {host}:{root}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SmtpConfig;

    fn config() -> SmtpConfig {
        SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            from: "spacetrace@example.com".into(),
            username: Some("user".into()),
            password: Some("secret".into()),
            password_file: None,
            accept_invalid_certs: false,
            timeout_seconds: 15,
        }
    }

    #[test]
    fn a_valid_configuration_builds() {
        let mailer = Mailer::build(&config()).expect("this configuration is usable");
        assert_eq!(mailer.describe.port, 587);
        assert!(mailer.describe.authenticated);
    }

    /// A typo in `from` must stop the hub at startup rather than at the first
    /// alert. The whole point of building the transport eagerly.
    #[test]
    fn a_from_address_that_is_not_an_address_is_refused() {
        let mut config = config();
        config.from = "not an address".into();
        let err = match Mailer::build(&config) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("an unparseable From address must not build"),
        };
        assert!(err.contains("smtp.from"), "unhelpful error: {err}");
    }

    /// What the settings page may show, as an assertion rather than as a
    /// habit. The password must not be reachable through the description.
    #[test]
    fn the_description_carries_no_secret() {
        let mailer = Mailer::build(&config()).unwrap();
        let json = serde_json::to_string(&mailer.describe).unwrap();
        assert!(!json.contains("secret"), "the password leaked: {json}");
        assert!(!json.contains("user"), "the username leaked: {json}");
    }

    /// Header injection, which is the reason this file uses a library. The
    /// address comes from a rule someone typed; a newline in it must not be
    /// able to start a header block of its own.
    #[tokio::test]
    async fn a_newline_in_the_recipient_is_refused() {
        let mailer = Mailer::build(&config()).unwrap();
        let err = mailer
            .send("ops@example.com\r\nBcc: elsewhere@example.com", "s", "b")
            .await
            .expect_err("a recipient with a CRLF in it is not an address");
        assert!(err.contains("not an email address"), "unexpected: {err}");
    }
}
