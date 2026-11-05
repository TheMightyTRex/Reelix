//! Discord incoming webhook with a rich embed.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "discord";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Discord",
        "Posts an embed to a Discord channel webhook",
        Some("https://support.discord.com/hc/en-us/articles/228383668"),
        vec![
            FieldSchema::new("webhook_url", "Webhook URL", FieldKind::Secret)
                .required()
                .placeholder("https://discord.com/api/webhooks/…"),
            FieldSchema::new("username", "Username", FieldKind::Text)
                .help("Overrides the webhook's default name"),
            FieldSchema::new("avatar_url", "Avatar URL", FieldKind::Url).advanced(),
            FieldSchema::new("include_poster", "Include images", FieldKind::Bool)
                .help("Poster thumbnail and backdrop image")
                .default_value(true),
        ],
    )
}

/// Discord webhook notifier.
pub struct Discord {
    http: reqwest::Client,
    webhook_url: String,
    username: String,
    avatar_url: Option<String>,
    include_poster: bool,
    events: EventMask,
}

impl Discord {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            webhook_url: s.url("webhook_url")?,
            username: s.opt_str("username").unwrap_or_else(|| ctx.app_name.clone()),
            avatar_url: s.opt_str("avatar_url"),
            include_poster: s.bool("include_poster"),
            events: s.events()?,
        })
    }

    /// The JSON body posted to the webhook. Media events use a rich layout: the media as
    /// the title, what happened as the description, overview, facts, links, release name,
    /// the backdrop as the image and the poster as the thumbnail.
    pub fn payload(&self, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let rich = !m.subject.is_empty();
        let mut fields: Vec<Value> = Vec::new();
        let mut field = |name: &str, value: String, inline: bool| {
            if value.is_empty() || fields.len() >= 25 {
                return;
            }
            let mut name = name.to_owned();
            truncate(&mut name, 256);
            let mut value = value;
            truncate(&mut value, 1024);
            fields.push(json!({ "name": name, "value": value, "inline": inline }));
        };
        if rich && let Some(overview) = &m.overview {
            let mut overview = overview.clone();
            truncate(&mut overview, 300);
            field("Overview", overview, false);
        }
        for f in &m.fields {
            let inline = f.value.chars().count() < 40;
            field(&f.name, f.value.clone(), inline);
        }
        if rich {
            let links: Vec<String> = m
                .links
                .iter()
                .map(|(name, url)| format!("[{name}]({url})"))
                .collect();
            field("Links", links.join(" / "), false);
            if let Some(release) = &m.release {
                field(
                    "Release",
                    format!("```\n{}\n```", release.replace("```", "")),
                    false,
                );
            }
            // Imports and file deletions keep the library path that used to be the
            // description. Other events show their text as details, unless it repeats the
            // overview (added, requests) or the release name (grabs).
            let body = m.body.trim();
            let repeated =
                m.release.as_deref() == Some(body) || m.overview.as_deref().map(str::trim) == Some(body);
            if !repeated {
                let name = match event {
                    NotificationEvent::Import { .. } | NotificationEvent::FileDeleted { .. } => "Path",
                    _ => "Details",
                };
                field(name, m.body.clone(), false);
            }
        }
        let (title, description) = if rich {
            (m.subject.clone(), m.headline.clone())
        } else {
            (m.title.clone(), m.body.clone())
        };
        let mut title = title;
        truncate(&mut title, 256);
        let mut description = description;
        truncate(&mut description, 4096);
        let mut embed = json!({
            "title": title,
            "description": description,
            "color": m.severity.color(),
            "fields": fields,
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "footer": { "text": self.username },
        });
        if rich {
            let mut author = json!({ "name": self.username });
            if let Some(a) = &self.avatar_url {
                author["icon_url"] = json!(a);
            }
            embed["author"] = author;
        }
        if let Some(u) = &m.url {
            embed["url"] = json!(u);
        }
        if self.include_poster {
            if let Some(img) = &m.image_url {
                embed["thumbnail"] = json!({ "url": img });
            }
            if let Some(img) = &m.backdrop_url {
                embed["image"] = json!({ "url": img });
            }
        }
        let mut body = json!({ "username": self.username, "embeds": [embed] });
        if let Some(a) = &self.avatar_url {
            body["avatar_url"] = json!(a);
        }
        body
    }
}

#[async_trait]
impl Notifier for Discord {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let resp = self
            .http
            .post(&self.webhook_url)
            .query(&[("wait", "true")])
            .json(&self.payload(event))
            .send()
            .await?;
        check(resp).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::NotificationEvent;

    fn discord() -> Discord {
        Discord {
            http: reqwest::Client::new(),
            webhook_url: "https://discord.example/api/webhooks/1/x".into(),
            username: "Reelix".into(),
            avatar_url: Some("https://example.com/icon.png".into()),
            include_poster: true,
            events: EventMask::all(),
        }
    }

    fn fields(body: &Value) -> Vec<(String, String, bool)> {
        body["embeds"][0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["name"].as_str().unwrap().to_owned(),
                    f["value"].as_str().unwrap().to_owned(),
                    f["inline"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn import_embed_matches_the_rich_layout() {
        let import = crate::samples::all()
            .into_iter()
            .find(|e| matches!(e, NotificationEvent::Import { .. }))
            .unwrap();
        let body = discord().payload(&import);
        let embed = &body["embeds"][0];
        assert_eq!(embed["author"]["icon_url"], "https://example.com/icon.png");
        assert_eq!(embed["title"], "The Lighthouse Keeper (2023)");
        let f = fields(&body);
        let get = |name: &str| f.iter().find(|(n, ..)| n == name).cloned();
        assert_eq!(f[0].0, "Overview");
        assert!(!f[0].2, "overview spans the embed");
        assert_eq!(get("Rating").unwrap().1, "7.4");
        assert_eq!(get("Genres").unwrap().1, "Drama, Mystery, Thriller");
        assert_eq!(
            get("Codecs").unwrap(),
            ("Codecs".into(), "h264 / DTS-HD MA 5.1".into(), true)
        );
        assert_eq!(get("Subtitles").unwrap().1, "English, Spanish");
        let links = get("Links").unwrap().1;
        for l in [
            "[TMDb](",
            "[IMDb](https://www.imdb.com/title/tt9123450/)",
            "[Trakt](",
            "[YouTube](",
        ] {
            assert!(links.contains(l), "{l} in {links}");
        }
        assert_eq!(
            get("Release").unwrap().1,
            "```\nThe.Lighthouse.Keeper.2023.1080p.BluRay.x264-NOCTURN\n```"
        );
        assert!(get("Path").unwrap().1.ends_with("Bluray-1080p.mkv"));
    }

    #[test]
    fn overview_is_not_repeated_as_a_path() {
        for event in crate::samples::all().into_iter().filter(|e| {
            matches!(
                e,
                NotificationEvent::MediaAdded { .. } | NotificationEvent::RequestApproved { .. }
            )
        }) {
            let f = fields(&discord().payload(&event));
            assert!(f.iter().filter(|(n, ..)| n == "Overview").count() <= 1, "{f:?}");
            assert!(f.iter().all(|(n, ..)| n != "Path"), "{f:?}");
            // The overview, when there is one, is shown once: never again under another name.
            if let Some((_, overview, _)) = f.iter().find(|(n, ..)| n == "Overview") {
                assert!(f.iter().filter(|(_, v, _)| v == overview).count() == 1, "{f:?}");
            }
        }
    }

    #[test]
    fn text_that_is_not_a_path_is_shown_as_details() {
        let deleted = crate::samples::all()
            .into_iter()
            .find(|e| matches!(e, NotificationEvent::MediaDeleted { .. }))
            .unwrap();
        let f = fields(&discord().payload(&deleted));
        assert!(f.iter().all(|(n, ..)| n != "Path"));
        assert!(
            f.iter()
                .any(|(n, v, _)| n == "Details" && v.starts_with("Removed from the library"))
        );
    }

    #[test]
    fn events_without_media_keep_the_plain_layout() {
        let health = crate::samples::all()
            .into_iter()
            .find(|e| matches!(e, NotificationEvent::HealthIssue(_)))
            .unwrap();
        let body = discord().payload(&health);
        let embed = &body["embeds"][0];
        assert_eq!(embed["title"], "Health issue");
        assert!(embed.get("author").is_none());
        assert!(fields(&body).iter().all(|(n, ..)| n != "Links"));
    }
}
