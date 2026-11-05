//! Reads and edits the `[server]` and `[logging]` keys of `config.toml`, keeping comments and
//! every other key as they are, and moves data-folder paths after a copy ([`relocate`]).

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use reelix_api_types::system::ServerSettingsValues;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::{self, Config, DEFAULT_PORT, LogFormat};

pub fn running(cfg: &Config) -> ServerSettingsValues {
    ServerSettingsValues {
        bind: cfg.bind.to_string(),
        port: cfg.port,
        url_base: cfg.url_base.clone(),
        log_level: cfg.log_level.clone(),
        log_format: format_str(cfg.log_format).into(),
    }
}

fn format_str(f: LogFormat) -> &'static str {
    match f {
        LogFormat::Pretty => "pretty",
        LogFormat::Json => "json",
    }
}

fn read_doc(path: &Path) -> anyhow::Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => config::default_config_template(),
        Err(e) => return Err(e.into()),
    };
    Ok(text.parse::<DocumentMut>()?)
}

/// Values saved in the config file; unset keys show their defaults.
pub fn saved(path: &Path) -> anyhow::Result<ServerSettingsValues> {
    let doc = read_doc(path)?;
    let get = |table: &str, key: &str| doc.get(table).and_then(|t| t.get(key));
    Ok(ServerSettingsValues {
        bind: get("server", "bind")
            .and_then(Item::as_str)
            .unwrap_or("0.0.0.0")
            .to_owned(),
        port: get("server", "port")
            .and_then(Item::as_integer)
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(DEFAULT_PORT),
        url_base: get("server", "url_base")
            .and_then(Item::as_str)
            .map(|u| config::normalize_url_base(u).unwrap_or_else(|_| u.to_owned()))
            .unwrap_or_default(),
        log_level: get("logging", "level")
            .and_then(Item::as_str)
            .unwrap_or("info")
            .to_owned(),
        log_format: get("logging", "format")
            .and_then(Item::as_str)
            .unwrap_or("pretty")
            .to_owned(),
    })
}

/// Validates and normalises `v`; the error is safe to show to the user.
pub fn validate(v: &ServerSettingsValues) -> Result<ServerSettingsValues, String> {
    let bind: IpAddr = v.bind.trim().parse().map_err(|_| {
        format!(
            "{:?} is not an IP address (use 0.0.0.0 for every interface)",
            v.bind
        )
    })?;
    if v.port == 0 {
        return Err("the port must be between 1 and 65535".into());
    }
    let url_base = config::normalize_url_base(&v.url_base)?;
    let log_level = v.log_level.trim().to_owned();
    if log_level.is_empty() {
        return Err("the log level can't be empty".into());
    }
    tracing_subscriber::EnvFilter::try_new(&log_level)
        .map_err(|e| format!("{log_level:?} is not a valid log level or filter: {e}"))?;
    let log_format = v.log_format.trim().to_ascii_lowercase();
    if log_format != "pretty" && log_format != "json" {
        return Err("the log format must be pretty or json".into());
    }
    Ok(ServerSettingsValues {
        bind: bind.to_string(),
        port: v.port,
        url_base,
        log_level,
        log_format,
    })
}

fn table<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    if !doc.contains_table(name) {
        doc.insert(name, Item::Table(Table::new()));
    }
    doc[name].as_table_mut().expect("just inserted a table")
}

/// Writes validated values into the config file (atomically).
pub fn save(path: &Path, v: &ServerSettingsValues) -> anyhow::Result<()> {
    let mut doc = read_doc(path)?;
    let server = table(&mut doc, "server");
    server["bind"] = value(v.bind.as_str());
    server["port"] = value(i64::from(v.port));
    server["url_base"] = value(v.url_base.as_str());
    let logging = table(&mut doc, "logging");
    logging["level"] = value(v.log_level.as_str());
    logging["format"] = value(v.log_format.as_str());
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.partial");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Keys that may point into the data directory.
const DATA_LOCATIONS: &[(&str, &str)] = &[
    ("paths", "data_dir"),
    ("paths", "backup_dir"),
    ("paths", "log_dir"),
    ("security", "secret_key_file"),
    ("database", "url"),
];

/// `path` with `/` separators, no trailing separator and (on Windows) ASCII lower case, for
/// comparing paths; byte offsets stay the same as in `path`.
fn comparable(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/');
    if cfg!(windows) {
        p.to_ascii_lowercase()
    } else {
        p.to_owned()
    }
}

/// `value` moved from under `old` to under `new`, if it is `old` or inside it.
fn moved(value: &str, old: &str, new: &Path) -> Option<PathBuf> {
    let (v, o) = (comparable(value), comparable(old));
    if o.is_empty() {
        return None;
    }
    if v == o {
        return Some(new.to_path_buf());
    }
    v.strip_prefix(&format!("{o}/"))?;
    let rest = value[o.len() + 1..].trim_start_matches(['/', '\\']);
    Some(new.join(rest))
}

/// The `sqlite:` URL with its file moved from under `old` to under `new`.
fn moved_sqlite_url(url: &str, old: &str, new: &Path) -> Option<String> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let file = moved(path, old, new)?;
    let mut out = format!("sqlite://{}", file.to_string_lossy().replace('\\', "/"));
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

/// After the data directory was copied from `old` to `new` (the Windows installer's migration
/// from another Reelix folder): rewrites the keys of the config file at `path` that point into
/// `old` so they point to the same place under `new`. Relative locations depended on the old
/// working directory: a relative `paths.data_dir` becomes `new`, the others are dropped so their
/// defaults under the new data directory apply. Comments and every other key stay as they are;
/// the file is only written when something changed. Returns the changed `table.key` names.
pub fn relocate(path: &Path, old: &Path, new: &Path) -> anyhow::Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut doc: DocumentMut = text.parse()?;
    let old = old.to_string_lossy();
    let mut changed = Vec::new();
    for &(table, key) in DATA_LOCATIONS {
        let Some(t) = doc.get_mut(table).and_then(Item::as_table_like_mut) else {
            continue;
        };
        let Some(current) = t.get(key).and_then(Item::as_str).map(str::to_owned) else {
            continue;
        };
        let relative = key != "url" && Path::new(&current).is_relative();
        let replacement = if key == "url" {
            moved_sqlite_url(&current, &old, new)
        } else {
            moved(&current, &old, new)
                .or_else(|| (relative && key == "data_dir").then(|| new.to_path_buf()))
                .map(|p| p.to_string_lossy().into_owned())
        };
        match replacement {
            Some(r) => {
                if let Some(v) = t.get_mut(key).and_then(Item::as_value_mut) {
                    let decor = v.decor().clone();
                    *v = toml_edit::Value::from(r);
                    *v.decor_mut() = decor;
                }
            }
            None if relative => {
                t.remove(key);
            }
            None => continue,
        }
        changed.push(format!("{table}.{key}"));
    }
    if !changed.is_empty() {
        let tmp = path.with_extension("toml.partial");
        std::fs::write(&tmp, doc.to_string())?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(changed)
}

/// Settings forced by `REELIX_*` / `RUST_LOG` environment variables or command-line flags.
pub fn overridden(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for (key, var) in [
        ("bind", "REELIX_BIND"),
        ("port", "REELIX_PORT"),
        ("url_base", "REELIX_URL_BASE"),
        ("log_level", "REELIX_LOG_LEVEL"),
        ("log_level", "RUST_LOG"),
        ("log_format", "REELIX_LOG_FORMAT"),
    ] {
        if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
            out.push(format!("{key} ({var})"));
        }
    }
    for key in &cfg.cli_overrides {
        if matches!(*key, "port" | "bind") {
            out.push(format!("{key} (--{key})"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> ServerSettingsValues {
        ServerSettingsValues {
            bind: "127.0.0.1".into(),
            port: 8080,
            url_base: "reelix/".into(),
            log_level: "debug".into(),
            log_format: "JSON".into(),
        }
    }

    #[test]
    fn validates_and_normalises() {
        let v = validate(&values()).unwrap();
        assert_eq!(v.url_base, "/reelix");
        assert_eq!(v.log_format, "json");
        assert!(
            validate(&ServerSettingsValues {
                bind: "nope".into(),
                ..values()
            })
            .is_err()
        );
        assert!(validate(&ServerSettingsValues { port: 0, ..values() }).is_err());
        assert!(
            validate(&ServerSettingsValues {
                url_base: "a b".into(),
                ..values()
            })
            .is_err()
        );
        assert!(
            validate(&ServerSettingsValues {
                log_format: "xml".into(),
                ..values()
            })
            .is_err()
        );
        assert!(
            validate(&ServerSettingsValues {
                log_level: "reelix=debug,info".into(),
                ..values()
            })
            .is_ok()
        );
    }

    #[test]
    fn relocate_moves_data_paths_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let old = dir.path().join("old").join("data");
        let new = dir.path().join("new");
        let o = old.to_string_lossy().replace('\\', "/");
        // An absolute path outside the old data folder (Windows needs a drive letter).
        let elsewhere = if cfg!(windows) {
            "C:/srv/backups"
        } else {
            "/srv/backups"
        };
        let text = format!(
            "# keep me\n[server]\nport = 7711\n\n[database]\nurl = \"sqlite://{o}/reelix.db?mode=rwc\"\n\n\
             [paths]\ndata_dir = \"./data\"\nlog_dir = '{o}/logs' # logs\nbackup_dir = \"{elsewhere}\"\n\
             web_dir = \"{o}/../web\"\n\n[security]\nsecret_key_file = \"data/secret.key\"\n"
        );
        std::fs::write(&path, text).unwrap();
        let changed = relocate(&path, &old, &new).unwrap();
        assert_eq!(
            changed,
            [
                "paths.data_dir",
                "paths.log_dir",
                "security.secret_key_file",
                "database.url"
            ]
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: DocumentMut = text.parse().unwrap();
        assert!(text.starts_with("# keep me"), "{text}");
        assert!(text.contains("# logs"), "{text}");
        assert_eq!(doc["server"]["port"].as_integer(), Some(7711));
        assert_eq!(doc["paths"]["data_dir"].as_str(), Some(&*new.to_string_lossy()));
        assert_eq!(
            doc["paths"]["log_dir"].as_str(),
            Some(&*new.join("logs").to_string_lossy())
        );
        // Outside the old data folder: untouched.
        assert_eq!(doc["paths"]["backup_dir"].as_str(), Some(elsewhere));
        assert!(doc["paths"]["web_dir"].as_str().unwrap().ends_with("../web"));
        // Relative: dropped, so the default (<data_dir>/secret.key) applies.
        assert!(doc["security"].get("secret_key_file").is_none());
        let n = new.to_string_lossy().replace('\\', "/");
        assert_eq!(
            doc["database"]["url"].as_str(),
            Some(&*format!("sqlite://{n}/reelix.db?mode=rwc"))
        );
        // Nothing left to change; no file: nothing to do.
        assert!(relocate(&path, &old, &new).unwrap().is_empty());
        assert!(
            relocate(&dir.path().join("none.toml"), &old, &new)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn saves_keeping_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# my comment\n[server]\nport = 7700\n\n[database]\nurl = \"sqlite:///x.db\"\n",
        )
        .unwrap();
        let v = validate(&values()).unwrap();
        save(&path, &v).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my comment"));
        assert!(text.contains("url = \"sqlite:///x.db\""));
        assert_eq!(saved(&path).unwrap(), v);

        // A missing file starts from the commented template.
        let fresh = dir.path().join("new.toml");
        assert_eq!(saved(&fresh).unwrap().port, DEFAULT_PORT);
        save(&fresh, &v).unwrap();
        let text = std::fs::read_to_string(&fresh).unwrap();
        assert!(text.contains("Reelix bootstrap configuration"));
        assert_eq!(saved(&fresh).unwrap(), v);
    }
}
