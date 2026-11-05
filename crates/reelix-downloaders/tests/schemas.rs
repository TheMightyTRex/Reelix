//! Every implementation exposes a schema and builds from its defaults (plus required fields).

use reelix_core::provider::ProviderSchema;
use reelix_downloaders::*;
use serde_json::json;

#[test]
fn every_implementation_builds_from_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = BuildContext::default();
    for schema in schemas() {
        let settings = match schema.implementation.as_str() {
            "sabnzbd" => json!({ "api_key": "k" }),
            "slskd" => json!({ "api_key": "k", "downloads_dir": "/app/downloads" }),
            "torrent_blackhole" | "usenet_blackhole" => {
                json!({ "drop_folder": tmp.path(), "watch_folder": tmp.path() })
            }
            _ => json!({}),
        };
        let c = build(&schema.implementation, &settings, &ctx)
            .unwrap_or_else(|e| panic!("{}: {e}", schema.implementation));
        assert_eq!(Some(c.protocol()), protocol_of(&schema.implementation));
        assert_eq!(c.info().implementation, schema.implementation);
        assert_eq!(
            c.info().name,
            schema.fields[0].default.as_ref().unwrap().as_str().unwrap()
        );
        assert!(c.info().remove_completed && c.info().remove_failed);
        // Schemas serialise for the UI and round-trip.
        let v = serde_json::to_value(&schema).unwrap();
        let back: ProviderSchema = serde_json::from_value(v).unwrap();
        assert_eq!(back, schema);
        // Field names are unique.
        let mut names: Vec<_> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), schema.fields.len(), "{}", schema.implementation);
    }
}

#[test]
fn invalid_settings_are_rejected() {
    let ctx = BuildContext::default();
    let bad = |imp: &str, s: serde_json::Value| {
        matches!(build(imp, &s, &ctx), Err(ClientError::InvalidSettings(_)))
    };
    assert!(bad("qbittorrent", json!({ "port": "not a port" })));
    assert!(bad("qbittorrent", json!({ "port": 70000 })));
    assert!(bad("transmission", json!({ "host": "bad host/x" })));
    assert!(bad(
        "sabnzbd",
        json!({ "api_key": "k", "recent_priority": "Urgent" })
    ));
    assert!(bad(
        "sabnzbd",
        json!({ "api_key": "k", "grab_method": "carrier_pigeon" })
    ));
    assert!(bad("nzbget", json!("not an object")));
    // Numeric strings from forms are accepted.
    assert!(build("deluge", &json!({ "port": "8112", "priority": "3" }), &ctx).is_ok());
}
