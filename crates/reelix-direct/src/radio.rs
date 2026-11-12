//! Radio catch-up (RFX-96): one episode's audio from its page on a known catch-up site, with yt-dlp, for the
//! radio service to file. Only these pages are accepted, the URL always goes after `--` so it can never be an
//! option, and the options are fixed: the best audio as it is, no playlists, plain file names.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::DirectService;
use crate::process::command;

/// The longest a catch-up fetch may take (a three-hour programme on a slow line).
const TIMEOUT: Duration = Duration::from_secs(3 * 3600);

/// The catch-up page, checked and normalised: BBC programme (episode) pages, NTS episodes, Mixcloud uploads and Global
/// Player catch-up episodes, over HTTPS, with no credentials or port.
pub fn catchup_page(page: &str) -> Result<String> {
    let url = url::Url::parse(page.trim()).context("Not a web address")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Use an https address without a user name or port");
    }
    let host = url.host_str().unwrap_or_default();
    let path = url.path();
    let ok = match host {
        // A BBC programme (episode) page: /programmes/{pid}, one segment after it.
        "www.bbc.co.uk" => path.starts_with("/programmes/") && path.trim_matches('/').split('/').count() == 2,
        "www.nts.live" | "nts.live" => path.starts_with("/shows/") && path.contains("/episodes/"),
        "www.mixcloud.com" | "mixcloud.com" => path.trim_matches('/').split('/').count() == 2,
        "www.globalplayer.com" | "globalplayer.com" => {
            path.starts_with("/catchup/") && path.contains("/episodes/")
        }
        _ => false,
    };
    if !ok {
        bail!("Not a catch-up page Reelix fetches");
    }
    let mut clean = url.clone();
    clean.set_query(None);
    clean.set_fragment(None);
    Ok(clean.into())
}

impl DirectService {
    /// Fetches the audio of one catch-up episode into `into` (an empty folder), with the yt-dlp from the direct
    /// download settings. Fails when yt-dlp is missing, the page is not one Reelix fetches, or nothing came.
    pub async fn fetch_radio_audio(&self, page: &str, into: &Path) -> Result<()> {
        let page = catchup_page(page)?;
        let settings = self.settings().await?;
        let exe = if settings.yt_dlp.trim().is_empty() {
            "yt-dlp"
        } else {
            settings.yt_dlp.trim()
        };
        let mut cmd = command(exe);
        cmd.arg("--no-playlist")
            .arg("--no-progress")
            .arg("--no-mtime")
            .arg("--restrict-filenames")
            .args(["-f", "bestaudio/best"])
            .args(["--ffmpeg-location", settings.ffmpeg.as_str()])
            .arg("-o")
            .arg(into.join("%(id)s.%(ext)s"))
            .arg("--")
            .arg(&page)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = cmd
            .spawn()
            .with_context(|| format!("yt-dlp could not start ({exe}); catch-up needs the full Reelix"))?;
        let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
            .await
            .context("the catch-up download took more than three hours")??;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            let last = said
                .lines()
                .rev()
                .find(|l| l.contains("ERROR"))
                .unwrap_or("yt-dlp failed")
                .trim();
            bail!("{}", last.chars().take(300).collect::<String>());
        }
        let mut dir = tokio::fs::read_dir(into).await?;
        while let Some(e) = dir.next_entry().await? {
            if e.file_type().await?.is_file() {
                return Ok(());
            }
        }
        bail!("yt-dlp finished but left no file")
    }
}

#[cfg(test)]
mod tests {
    use super::catchup_page;

    #[test]
    fn only_catchup_pages() {
        for ok in [
            "https://www.bbc.co.uk/programmes/m0033s7j",
            "https://www.nts.live/shows/100-elements/episodes/100-elements-9th-october-2026",
            "https://www.mixcloud.com/worldwidefm/nossa-12-11-26/",
            "https://www.globalplayer.com/catchup/lbc/uk/episodes/BUoaj2kK4yF2hCzgxSvo6ic9s/",
        ] {
            assert!(catchup_page(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://www.bbc.co.uk/programmes/m0033s7j",
            "https://www.bbc.co.uk/programmes/m0033s7j/episodes/guide",
            "https://www.bbc.co.uk/iplayer/episode/x",
            "https://user:pw@www.mixcloud.com/a/b/",
            "https://www.mixcloud.com/worldwidefm/",
            "https://www.nts.live/shows/x",
            "https://evil.example/sounds/play/x",
            "--exec=rm",
        ] {
            assert!(catchup_page(bad).is_err(), "{bad}");
        }
        assert_eq!(
            catchup_page("https://www.mixcloud.com/a/b/?utm=1#x").unwrap(),
            "https://www.mixcloud.com/a/b/"
        );
    }
}
