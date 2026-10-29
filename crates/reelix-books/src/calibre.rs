//! Optional Calibre integration: imported ebooks are also added with `calibredb add`.

use std::path::Path;
use std::time::Duration;

use reelix_api_types::books::CalibreSettingsDto;

/// Runs `calibredb add --library-path <lib> <file>`; returns the Calibre book id. Failures
/// are returned as text for the caller to log (they never fail an import).
pub(crate) async fn add(c: &CalibreSettingsDto, file: &Path) -> Result<Option<i64>, String> {
    let Some(lib) = c.library_path.as_deref().filter(|l| !l.trim().is_empty()) else {
        return Err("calibre library_path is not set".into());
    };
    let bin = if c.calibredb_path.trim().is_empty() {
        "calibredb"
    } else {
        c.calibredb_path.trim()
    };
    let run = tokio::process::Command::new(bin)
        .arg("add")
        .arg("--library-path")
        .arg(lib)
        .arg(file)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(300), run)
        .await
        .map_err(|_| "calibredb timed out".to_string())?
        .map_err(|e| format!("running {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("calibredb failed ({}): {}", out.status, stderr.trim()));
    }
    Ok(parse_added_id(&stdout))
}

/// `"Added book ids: 12"` → 12.
fn parse_added_id(stdout: &str) -> Option<i64> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("Added book ids:"))
        .and_then(|ids| ids.split(',').next())
        .and_then(|id| id.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids() {
        assert_eq!(
            parse_added_id("Backing up metadata\nAdded book ids: 12\n"),
            Some(12)
        );
        assert_eq!(parse_added_id("Added book ids: 3, 4"), Some(3));
        assert_eq!(parse_added_id("nothing"), None);
    }

    #[tokio::test]
    async fn missing_binary_is_an_error() {
        let c = CalibreSettingsDto {
            enabled: true,
            library_path: Some("/tmp/lib".into()),
            calibredb_path: "/nonexistent/calibredb".into(),
        };
        assert!(add(&c, Path::new("/tmp/x.epub")).await.is_err());
    }
}
