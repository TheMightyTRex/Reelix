//! Browser-driven Spotify PKCE login for the web UI. Spotify only accepts the loopback
//! callback for librespot's client ID, so Reelix listens on it for a browser on this
//! machine and otherwise accepts the redirected address pasted from the address bar.
//! Only the reusable session is saved, never a password or OAuth token.
use crate::DirectService;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use librespot_core::SessionConfig;
use rand::RngCore;
use reelix_api_types::direct::*;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const LISTEN_ADDRESS: &str = "127.0.0.1:8898";
const SCOPES: &str = "streaming user-read-private";
const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const ACCOUNTS_URL: &str = "https://accounts.spotify.com";

pub(crate) struct PendingLogin {
    state: String,
    verifier: String,
    started: Instant,
    listener: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct LoginState {
    pending: Option<PendingLogin>,
    error: Option<String>,
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The `code` and `state` from a redirected address. A bare query string also works.
pub(crate) fn parse_redirect(value: &str) -> Result<(String, String)> {
    let value = value.trim();
    let url = url::Url::parse(value)
        .or_else(|_| url::Url::parse(&format!("{REDIRECT_URI}?{}", value.trim_start_matches('?'))))
        .context("Paste the full address from the browser's address bar")?;
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if let Some(error) = param("error") {
        bail!("Spotify did not authorize Reelix ({error})");
    }
    match (param("code"), param("state")) {
        (Some(code), Some(state)) if !code.is_empty() => Ok((code, state)),
        _ => bail!("That address has no Spotify authorization code; paste the address Spotify redirected to"),
    }
}

impl DirectService {
    fn managed_credentials(&self) -> PathBuf {
        self.data_dir.join("spotify-credentials.json")
    }

    fn login_state(&self) -> std::sync::MutexGuard<'_, LoginState> {
        self.login.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start a login, replacing any pending one, and listen on the loopback callback
    /// when the port is free.
    pub async fn spotify_login_start(self: &Arc<Self>) -> Result<SpotifyLoginStart> {
        let state = random_token(24);
        let verifier = random_token(64);
        let client_id = SessionConfig::default().client_id;
        let mut auth = url::Url::parse(&format!("{ACCOUNTS_URL}/authorize"))?;
        auth.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPES)
            .append_pair("state", &state)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge(&verifier));
        // Release the port held by a previous attempt before binding again.
        let old = self.login_state().pending.take().and_then(|p| p.listener);
        if let Some(old) = old {
            old.abort();
            let _ = old.await;
        }
        let listener = match TcpListener::bind(LISTEN_ADDRESS).await {
            Ok(listener) => Some(tokio::spawn(self.clone().listen(listener))),
            Err(e) => {
                tracing::info!(error = %e, "Spotify login callback port is busy; waiting for a pasted address");
                None
            }
        };
        let automatic = listener.is_some();
        *self.login_state() = LoginState {
            pending: Some(PendingLogin {
                state,
                verifier,
                started: Instant::now(),
                listener,
            }),
            error: None,
        };
        Ok(SpotifyLoginStart {
            auth_url: auth.into(),
            automatic,
        })
    }

    /// Finish with an address pasted from the browser.
    pub async fn spotify_login_finish(&self, redirect_url: &str) -> Result<SpotifyStatus> {
        let (code, state) = parse_redirect(redirect_url)?;
        let (verifier, listener) = self.claim_login(&state)?;
        if let Some(listener) = listener {
            listener.abort();
        }
        self.complete_login(&code, &verifier).await?;
        self.spotify_status().await
    }

    /// Take the pending login if `state` matches, so each authorization is used once.
    fn claim_login(&self, state: &str) -> Result<(String, Option<JoinHandle<()>>)> {
        let mut guard = self.login_state();
        let pending = guard
            .pending
            .as_ref()
            .filter(|p| p.started.elapsed() < LOGIN_TTL)
            .context("No Spotify login is in progress; click Connect Spotify again")?;
        if pending.state != state {
            bail!("That address belongs to a different Spotify login; use the newest one");
        }
        let pending = guard.pending.take().context("Spotify login already finished")?;
        Ok((pending.verifier, pending.listener))
    }

    async fn complete_login(&self, code: &str, verifier: &str) -> Result<String> {
        let client_id = SessionConfig::default().client_id;
        let token = self
            .exchange_code(&self.spotify_accounts, &client_id, code, verifier)
            .await?;
        let path = self.managed_credentials();
        let username = crate::spotify::save_session(&path, SessionConfig::default(), &token).await?;
        let _guard = self.gate.lock().await;
        let mut settings = self.settings().await?;
        settings.spotify_credentials = path.to_string_lossy().into_owned();
        self.db.settings().set(crate::SETTINGS, &settings).await?;
        tracing::info!(%username, "Spotify account connected for direct downloads");
        Ok(username)
    }

    pub(crate) async fn exchange_code(
        &self,
        accounts: &str,
        client_id: &str,
        code: &str,
        verifier: &str,
    ) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Token {
            access_token: String,
        }
        let response = self
            .http
            .post(format!("{accounts}/api/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", REDIRECT_URI),
                ("client_id", client_id),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| anyhow!("Cannot reach Spotify to finish the login"))?;
        if !response.status().is_success() {
            bail!(
                "Spotify rejected the authorization code (HTTP {}); click Connect Spotify again",
                response.status().as_u16()
            );
        }
        Ok(response
            .json::<Token>()
            .await
            .context("Spotify returned an unreadable token")?
            .access_token)
    }

    /// Serve the loopback callback until a login completes or the attempt expires.
    async fn listen(self: Arc<Self>, listener: TcpListener) {
        let served = tokio::time::timeout(LOGIN_TTL, async {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut buf = vec![0u8; 8 * 1024];
                let mut len = 0;
                while len < buf.len() && !buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                    match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf[len..])).await {
                        Ok(Ok(n)) if n > 0 => len += n,
                        _ => break,
                    }
                }
                let request = String::from_utf8_lossy(&buf[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("GET "))
                    .and_then(|l| l.split_whitespace().next())
                    .unwrap_or_default();
                if !target.starts_with("/login") {
                    let _ = stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    continue;
                }
                let outcome = match parse_redirect(&format!("http://127.0.0.1:8898{target}")) {
                    Ok((code, state)) => match self.claim_login(&state) {
                        Ok((verifier, _)) => self.complete_login(&code, &verifier).await,
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };
                let (done, title, detail) = match &outcome {
                    Ok(username) => (true, "Spotify connected", format!("Signed in as {username}. You can close this tab and return to Reelix.")),
                    Err(e) => (false, "Spotify login failed", format!("{e}. Return to Reelix to try again.")),
                };
                if let Err(e) = &outcome {
                    self.login_state().error = Some(e.to_string());
                }
                let body = format!(
                    "<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font-family:system-ui;margin:3rem\"><h1>{title}</h1><p>{}</p>",
                    html_escape(&detail)
                );
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                // A failed exchange consumed the login, so stop either way once the code arrived.
                if done || self.login_state().pending.is_none() {
                    return;
                }
            }
        })
        .await;
        if served.is_err() {
            let mut state = self.login_state();
            if state
                .pending
                .as_ref()
                .is_some_and(|p| p.started.elapsed() >= LOGIN_TTL)
            {
                state.pending = None;
                state.error = Some("The Spotify login expired; click Connect Spotify again".into());
            }
        }
    }

    pub async fn spotify_status(&self) -> Result<SpotifyStatus> {
        let settings = self.settings().await?;
        let credentials = if settings.spotify_credentials.is_empty() {
            None
        } else {
            crate::spotify::read_credentials(std::path::Path::new(&settings.spotify_credentials))
                .await
                .ok()
        };
        let state = self.login_state();
        Ok(SpotifyStatus {
            connected: credentials.is_some(),
            username: credentials.and_then(|c| c.username),
            credentials_path: settings.spotify_credentials,
            pending: state
                .pending
                .as_ref()
                .is_some_and(|p| p.started.elapsed() < LOGIN_TTL),
            error: state.error.clone(),
        })
    }

    /// Forget the account. Only the file Reelix created is deleted; a file configured
    /// by hand is left in place.
    pub async fn spotify_disconnect(&self) -> Result<SpotifyStatus> {
        let listener = self.login_state().pending.take().and_then(|p| p.listener);
        if let Some(listener) = listener {
            listener.abort();
        }
        {
            let _guard = self.gate.lock().await;
            let mut settings = self.settings().await?;
            let managed = self.managed_credentials();
            if std::path::Path::new(&settings.spotify_credentials) == managed {
                match tokio::fs::remove_file(&managed).await {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
            settings.spotify_credentials.clear();
            self.db.settings().set(crate::SETTINGS, &settings).await?;
        }
        self.login_state().error = None;
        self.spotify_status().await
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
