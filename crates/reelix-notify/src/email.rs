//! SMTP email via `lettre` (STARTTLS, implicit TLS or plain), multipart text + HTML.

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use serde_json::{Value, json};

use crate::common::{Settings, schema as mk_schema};
use crate::message::escape_html;
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "email";

/// Transport security for the SMTP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// Plain connection upgraded with STARTTLS (required).
    StartTls,
    /// Implicit TLS (SMTPS, usually port 465).
    Tls,
    /// No encryption (LAN relays only).
    None,
}

pub fn schema() -> ProviderSchema {
    let opt = |v: &str, l: &str| SelectOption {
        value: json!(v),
        label: l.into(),
    };
    mk_schema(
        ID,
        "Email",
        "Sends email through an SMTP server",
        None,
        vec![
            FieldSchema::new("server", "SMTP server", FieldKind::Text)
                .required()
                .placeholder("smtp.example.com"),
            FieldSchema::new("port", "Port", FieldKind::Number).default_value(587),
            FieldSchema::new(
                "encryption",
                "Encryption",
                FieldKind::Select {
                    options: vec![
                        opt("starttls", "STARTTLS"),
                        opt("tls", "TLS (implicit)"),
                        opt("none", "None"),
                    ],
                },
            )
            .default_value("starttls"),
            FieldSchema::new("username", "Username", FieldKind::Text),
            FieldSchema::new("password", "Password", FieldKind::Secret),
            FieldSchema::new("from", "From address", FieldKind::Text)
                .required()
                .placeholder("Reelix <reelix@example.com>"),
            FieldSchema::new("to", "To", FieldKind::List).required(),
            FieldSchema::new("cc", "CC", FieldKind::List).advanced(),
            FieldSchema::new("bcc", "BCC", FieldKind::List).advanced(),
            FieldSchema::new("subject_prefix", "Subject prefix", FieldKind::Text)
                .default_value("[Reelix]")
                .advanced(),
        ],
    )
}

fn mailbox(s: &str) -> Result<Mailbox, NotifyError> {
    s.parse()
        .map_err(|e| NotifyError::settings(format!("invalid email address {s:?}: {e}")))
}

fn mailboxes(list: Vec<String>) -> Result<Vec<Mailbox>, NotifyError> {
    list.iter().map(|s| mailbox(s)).collect()
}

/// SMTP notifier.
pub struct Email {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    encryption: Encryption,
    from: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    subject_prefix: Option<String>,
    events: EventMask,
}

impl Email {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let server = s.str("server")?;
        let port = u16::try_from(s.opt_i64("port").unwrap_or(587))
            .map_err(|_| NotifyError::settings("port out of range"))?;
        let encryption = match s.opt_str("encryption").as_deref() {
            Some("tls") => Encryption::Tls,
            Some("none") => Encryption::None,
            Some("starttls") | None => Encryption::StartTls,
            Some(o) => return Err(NotifyError::settings(format!("unknown encryption {o}"))),
        };
        let email_err = |e: lettre::transport::smtp::Error| NotifyError::Email(e.to_string());
        let mut builder = match encryption {
            Encryption::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server).map_err(email_err)?
            }
            Encryption::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&server).map_err(email_err)?,
            Encryption::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server),
        }
        .port(port)
        .timeout(Some(ctx.timeout));
        if let Some(user) = s.opt_str("username") {
            builder = builder.credentials(Credentials::new(user, s.opt_str("password").unwrap_or_default()));
        }
        let to = mailboxes(s.list("to"))?;
        if to.is_empty() {
            return Err(NotifyError::settings("at least one recipient is required"));
        }
        Ok(Self {
            transport: builder.build(),
            encryption,
            from: mailbox(&s.str("from")?)?,
            to,
            cc: mailboxes(s.list("cc"))?,
            bcc: mailboxes(s.list("bcc"))?,
            subject_prefix: s.opt_str("subject_prefix"),
            events: s.events()?,
        })
    }

    pub fn encryption(&self) -> Encryption {
        self.encryption
    }

    /// Builds the MIME message for an event (multipart/alternative: text + HTML).
    pub fn build_message(&self, event: &NotificationEvent) -> Result<lettre::Message, NotifyError> {
        let m = Message::from_event(event);
        let subject = match &self.subject_prefix {
            Some(p) => format!("{p} {}", m.title),
            None => m.title.clone(),
        };
        let mut text = m.text();
        if let Some(u) = &m.url {
            text.push_str("\n\n");
            text.push_str(u);
        }
        let mut html = format!(
            "<!doctype html><html><body style=\"font-family:sans-serif\"><h2>{}</h2>",
            escape_html(&m.title)
        );
        if let Some(img) = &m.image_url {
            html.push_str(&format!(
                "<img src=\"{}\" alt=\"\" style=\"max-width:150px;float:right\">",
                escape_html(img)
            ));
        }
        if !m.body.is_empty() {
            html.push_str(&format!("<p>{}</p>", escape_html(&m.body)));
        }
        if !m.fields.is_empty() {
            html.push_str("<table>");
            for f in &m.fields {
                html.push_str(&format!(
                    "<tr><th align=\"left\">{}</th><td>{}</td></tr>",
                    escape_html(&f.name),
                    escape_html(&f.value)
                ));
            }
            html.push_str("</table>");
        }
        if let Some(u) = &m.url {
            html.push_str(&format!("<p><a href=\"{0}\">{0}</a></p>", escape_html(u)));
        }
        html.push_str("</body></html>");

        let mut b = lettre::Message::builder()
            .from(self.from.clone())
            .subject(subject);
        for t in &self.to {
            b = b.to(t.clone());
        }
        for c in &self.cc {
            b = b.cc(c.clone());
        }
        for c in &self.bcc {
            b = b.bcc(c.clone());
        }
        b.multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text))
                .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(html)),
        )
        .map_err(|e| NotifyError::Email(e.to_string()))
    }
}

#[async_trait]
impl Notifier for Email {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let msg = self.build_message(event)?;
        self.transport
            .send(msg)
            .await
            .map_err(|e| NotifyError::Email(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples;

    fn ctx() -> BuildContext {
        BuildContext::new(reqwest::Client::new())
    }

    fn settings() -> Value {
        json!({
            "server": "smtp.example.com",
            "username": "mailer",
            "password": "pw",
            "from": "Reelix <reelix@example.com>",
            "to": ["alice@example.com", "Bob <bob@example.com>"],
            "bcc": "audit@example.com",
        })
    }

    #[test]
    fn builds_multipart_message_with_escaped_html() {
        let email = Email::from_settings(&settings(), &ctx()).unwrap();
        assert_eq!(email.encryption(), Encryption::StartTls);
        let ev = &samples::all()[1]; // grab
        let msg = email.build_message(ev).unwrap();
        let env = msg.envelope();
        assert_eq!(env.from().unwrap().to_string(), "reelix@example.com");
        // To + BCC are envelope recipients; BCC never appears in headers.
        assert_eq!(env.to().len(), 3);
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("Subject: [Reelix] Grabbed: The Lighthouse Keeper (2023)"));
        assert!(raw.contains("To: alice@example.com, Bob <bob@example.com>"));
        assert!(!raw.contains("audit@example.com"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/plain"));
        assert!(raw.contains("text/html"));
        assert!(raw.contains("Bluray-1080p"));
    }

    #[test]
    fn html_is_escaped() {
        let email = Email::from_settings(&settings(), &ctx()).unwrap();
        let ev = NotificationEvent::Test {
            message: "<script>alert(1)</script>".into(),
        };
        let raw = String::from_utf8(email.build_message(&ev).unwrap().formatted()).unwrap();
        let html = raw.split("text/html").nth(1).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn rejects_bad_addresses_and_encryption() {
        let mut s = settings();
        s["to"] = json!(["not an address"]);
        assert!(matches!(
            Email::from_settings(&s, &ctx()),
            Err(NotifyError::InvalidSettings(_))
        ));
        let mut s = settings();
        s["encryption"] = json!("ssl3");
        assert!(Email::from_settings(&s, &ctx()).is_err());
        let mut s = settings();
        s["encryption"] = json!("none");
        s["port"] = json!(25);
        assert_eq!(
            Email::from_settings(&s, &ctx()).unwrap().encryption(),
            Encryption::None
        );
    }

    #[test]
    fn every_event_builds() {
        let email = Email::from_settings(&settings(), &ctx()).unwrap();
        for ev in samples::all() {
            email.build_message(&ev).unwrap();
        }
    }
}
