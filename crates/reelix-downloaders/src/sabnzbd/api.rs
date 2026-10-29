//! Raw SABnzbd API calls: `{base}/api?apikey=…&output=json&mode=…`.

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::Sabnzbd;
use super::models::*;
use crate::ClientError;
use crate::common::{ok_text, snippet};

/// How many history entries are read per category.
pub const HISTORY_LIMIT: u32 = 100;

impl Sabnzbd {
    fn request(&self, method: reqwest::Method, params: &[(&str, &str)]) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, format!("{}/api", self.base_url))
            .query(&[("apikey", self.settings.api_key.as_str()), ("output", "json")])
            .query(params);
        if let Some(user) = self.settings.username.as_deref().filter(|u| !u.is_empty()) {
            rb = rb.basic_auth(user, self.settings.password.as_deref());
        }
        rb
    }

    /// Sends a request and decodes the JSON body, mapping SABnzbd error envelopes.
    async fn send<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> Result<T, ClientError> {
        let body = ok_text(rb.send().await?).await?;
        decode(&body)
    }

    pub(super) async fn call<T: DeserializeOwned>(&self, params: &[(&str, &str)]) -> Result<T, ClientError> {
        self.send(self.request(reqwest::Method::GET, params)).await
    }

    pub(super) async fn version(&self) -> Result<String, ClientError> {
        let v: VersionResponse = self.call(&[("mode", "version")]).await?;
        Ok(v.version)
    }

    /// `mode=get_config` (whole config or one section). Requires the full API key.
    pub(super) async fn get_config(&self, section: Option<&str>) -> Result<Value, ClientError> {
        let mut p = vec![("mode", "get_config")];
        if let Some(s) = section {
            p.push(("section", s));
        }
        let c: ConfigResponse = self.call(&p).await?;
        Ok(c.config)
    }

    pub(super) async fn get_cats(&self) -> Result<Vec<String>, ClientError> {
        let c: CategoriesResponse = self.call(&[("mode", "get_cats")]).await?;
        Ok(c.categories)
    }

    /// True when the configured key is accepted for adding jobs only (the NZB key):
    /// `addurl` without a URL fails with a parameter error rather than an auth error.
    pub(super) async fn is_nzb_key(&self) -> bool {
        matches!(
            self.call::<StatusResponse>(&[("mode", "addurl")]).await,
            Ok(_) | Err(ClientError::Api(_))
        )
    }

    pub(super) async fn add_file(
        &self,
        nzb: Vec<u8>,
        file_name: &str,
        nzbname: &str,
        cat: Option<&str>,
        priority: i32,
    ) -> Result<AddResponse, ClientError> {
        let part = reqwest::multipart::Part::bytes(nzb)
            .file_name(file_name.to_owned())
            .mime_str("application/x-nzb")
            .map_err(|e| ClientError::InvalidPayload(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("name", part);
        let prio = priority.to_string();
        let mut p = vec![("mode", "addfile"), ("priority", prio.as_str())];
        if let Some(c) = cat {
            p.push(("cat", c));
        }
        if !nzbname.is_empty() {
            p.push(("nzbname", nzbname));
        }
        self.send(self.request(reqwest::Method::POST, &p).multipart(form))
            .await
    }

    pub(super) async fn add_url(
        &self,
        url: &str,
        nzbname: &str,
        cat: Option<&str>,
        priority: i32,
    ) -> Result<AddResponse, ClientError> {
        let prio = priority.to_string();
        let mut p = vec![("mode", "addurl"), ("name", url), ("priority", prio.as_str())];
        if let Some(c) = cat {
            p.push(("cat", c));
        }
        if !nzbname.is_empty() {
            p.push(("nzbname", nzbname));
        }
        self.call(&p).await
    }

    pub(super) async fn queue(&self, cat: Option<&str>, nzo_id: Option<&str>) -> Result<Queue, ClientError> {
        let mut p = vec![("mode", "queue"), ("start", "0"), ("limit", "0")];
        if let Some(c) = cat {
            p.push(("cat", c));
        }
        if let Some(id) = nzo_id {
            p.push(("nzo_ids", id));
        }
        let q: QueueResponse = self.call(&p).await?;
        Ok(q.queue)
    }

    pub(super) async fn history(&self, cat: Option<&str>) -> Result<History, ClientError> {
        let limit = HISTORY_LIMIT.to_string();
        let mut p = vec![("mode", "history"), ("start", "0"), ("limit", limit.as_str())];
        if let Some(c) = cat {
            p.push(("category", c));
        }
        let h: HistoryResponse = self.call(&p).await?;
        Ok(h.history)
    }

    pub(super) async fn delete(
        &self,
        from_queue: bool,
        nzo_id: &str,
        del_files: bool,
    ) -> Result<(), ClientError> {
        let del = if del_files { "1" } else { "0" };
        let mut p = vec![
            ("mode", if from_queue { "queue" } else { "history" }),
            ("name", "delete"),
            ("value", nzo_id),
            ("del_files", del),
        ];
        if !from_queue {
            p.push(("archive", "0"));
        }
        let _: StatusResponse = self.call(&p).await?;
        Ok(())
    }

    pub(super) async fn queue_action(
        &self,
        name: &str,
        nzo_id: &str,
        value2: Option<&str>,
    ) -> Result<(), ClientError> {
        let mut p = vec![("mode", "queue"), ("name", name), ("value", nzo_id)];
        if let Some(v) = value2 {
            p.push(("value2", v));
        }
        let _: Value = self.call(&p).await?;
        Ok(())
    }

    /// `mode=warnings`; 3.x+ returns objects, older builds plain strings.
    pub(super) async fn warnings(&self) -> Result<Vec<String>, ClientError> {
        let v: Value = self.call(&[("mode", "warnings")]).await?;
        let list = v
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(list
            .into_iter()
            .filter_map(|w| match w {
                Value::String(s) => Some(s),
                Value::Object(o) => {
                    let text = o.get("text").and_then(Value::as_str)?.to_owned();
                    match o.get("type").and_then(Value::as_str) {
                        Some(t) => Some(format!("{t}: {text}")),
                        None => Some(text),
                    }
                }
                _ => None,
            })
            .collect())
    }
}

/// Decodes a body, turning `{"status": false, "error": …}` and plain-text errors into typed errors.
pub fn decode<T: DeserializeOwned>(body: &str) -> Result<T, ClientError> {
    let trimmed = body.trim();
    let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
        // Without output=json support (or on auth failures) SABnzbd answers in plain text.
        if let Some(msg) = trimmed.strip_prefix("error:") {
            return Err(classify_error(msg.trim()));
        }
        return Err(ClientError::Parse(format!("not JSON: {}", snippet(trimmed))));
    };
    if v.get("status").and_then(Value::as_bool) == Some(false) {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        return Err(classify_error(&msg));
    }
    serde_json::from_value(v).map_err(|e| ClientError::Parse(e.to_string()))
}

fn classify_error(msg: &str) -> ClientError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("api key") || lower.contains("apikey") || lower.contains("authentication") {
        ClientError::Auth(msg.to_owned())
    } else {
        ClientError::Api(msg.to_owned())
    }
}
