//! Reads and edits the `[server]` and `[logging]` keys of `config.toml`, keeping comments and
//! every other key as they are.

use std::net::IpAddr;
use std::path::Path;

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
