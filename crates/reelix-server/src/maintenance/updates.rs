//! Update checks against the project's GitHub releases.

use chrono::{DateTime, Utc};
use reelix_api_types::system::{ReleaseAssetDto, ReleaseDto};
use serde::Deserialize;

/// GitHub repository releases are published to.
pub const REPOSITORY: &str = "mvanhare/Reelix";
pub const GITHUB_API: &str = "https://api.github.com";
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn parse_version(v: &str) -> Option<semver::Version> {
    semver::Version::parse(v.trim().trim_start_matches(['v', 'V'])).ok()
}

/// True when `candidate` is a newer version than `current` (false if either doesn't parse).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(c), Some(r)) if c > r)
}

/// Fetches recent releases, newest first (drafts skipped).
pub async fn fetch(http: &reqwest::Client, api_base: &str, current: &str) -> Result<Vec<ReleaseDto>, String> {
    let url = format!(
        "{}/repos/{REPOSITORY}/releases?per_page=20",
        api_base.trim_end_matches('/')
    );
    let resp = http
        .get(&url)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "the repository {REPOSITORY} was not found on GitHub (is it public?)"
        ));
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("GitHub's rate limit was reached; try again later".into());
    }
    if !status.is_success() {
        return Err(format!("GitHub returned HTTP {status}"));
    }
    let releases: Vec<GhRelease> = resp
        .json()
        .await
        .map_err(|e| format!("unexpected response from GitHub: {e}"))?;
    let mut out: Vec<ReleaseDto> = releases
        .into_iter()
        .filter(|r| !r.draft)
        .map(|r| {
            let version = r.tag_name.trim().trim_start_matches(['v', 'V']).to_owned();
            ReleaseDto {
                newer: is_newer(&version, current),
                name: r
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| r.tag_name.clone()),
                version,
                notes: r.body.unwrap_or_default(),
                url: r.html_url,
                published_at: r.published_at,
                prerelease: r.prerelease,
                assets: r
                    .assets
                    .into_iter()
                    .map(|a| ReleaseAssetDto {
                        name: a.name,
                        url: a.browser_download_url,
                        size_bytes: a.size,
                    })
                    .collect(),
            }
        })
        .collect();
    // Highest version first; releases without a semver tag keep GitHub's (newest first) order at the end.
    out.sort_by(
        |a, b| match (parse_version(&a.version), parse_version(&b.version)) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("0.1.0", "0.1.0-alpha.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[tokio::test]
    async fn fetches_and_orders_releases() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPOSITORY}/releases")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"tag_name": "v0.1.0", "name": "", "body": "First", "html_url": "https://x/0.1.0",
                 "published_at": "2026-10-01T00:00:00Z", "prerelease": false, "draft": false, "assets": []},
                {"tag_name": "v0.3.0", "name": "Draft", "html_url": "https://x/d", "draft": true},
                {"tag_name": "v0.2.0", "name": "Reelix 0.2.0", "body": "Notes", "html_url": "https://x/0.2.0",
                 "published_at": "2026-11-01T00:00:00Z", "prerelease": true, "draft": false,
                 "assets": [{"name": "reelix-0.2.0-windows-x86_64.zip", "browser_download_url": "https://x/a.zip", "size": 42}]}
            ])))
            .mount(&server)
            .await;
        let http = reqwest::Client::builder().user_agent("test").build().unwrap();
        let releases = fetch(&http, &server.uri(), "0.1.0").await.unwrap();
        assert_eq!(releases.len(), 2, "drafts are skipped");
        assert_eq!(releases[0].version, "0.2.0");
        assert!(releases[0].newer && releases[0].prerelease);
        assert_eq!(releases[0].assets[0].size_bytes, 42);
        assert_eq!(releases[1].name, "v0.1.0", "empty names fall back to the tag");
        assert!(!releases[1].newer);
    }

    #[tokio::test]
    async fn reports_missing_repositories() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let err = fetch(&http, &server.uri(), "0.1.0").await.unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }
}
