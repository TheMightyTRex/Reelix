//! Settings schemas per provider kind, as served to the UI and used for validation.
//!
//! Indexer schemas gain Reelix-level fields (RSS/search toggles, rate limits, seeding goals);
//! download-client schemas lose `name`/`priority`/`tags`, which live on the provider row and
//! are injected when the client is built.

use reelix_api_types::providers::{ProviderKind, ProviderSchemaDto};
use reelix_core::Protocol;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use reelix_indexers::DefinitionStore;
use reelix_indexers::definition::IMPLEMENTATION_PREFIX;

/// Download-client fields that duplicate provider row columns.
pub const ROW_FIELDS: [&str; 3] = ["name", "priority", "tags"];

fn indexer_fields(protocol: Protocol) -> Vec<FieldSchema> {
    // Soulseek has no feed of new releases.
    let rss = protocol != Protocol::Soulseek;
    let mut v = vec![
        FieldSchema::new("enable_rss", "Enable RSS", FieldKind::Bool)
            .default_value(rss)
            .help(if rss {
                "Use this indexer for RSS sync"
            } else {
                "Soulseek has no RSS feed; this indexer is only used by searches"
            }),
        FieldSchema::new(
            "enable_automatic_search",
            "Enable automatic search",
            FieldKind::Bool,
        )
        .default_value(true)
        .help("Used when Reelix searches for wanted items"),
        FieldSchema::new(
            "enable_interactive_search",
            "Enable interactive search",
            FieldKind::Bool,
        )
        .default_value(true)
        .help("Used for manual and interactive searches"),
        FieldSchema::new("query_limit", "Query limit", FieldKind::Number)
            .help("Maximum queries per period; empty for unlimited")
            .advanced(),
        FieldSchema::new(
            "query_limit_hours",
            "Query limit period (hours)",
            FieldKind::Number,
        )
        .default_value(24)
        .advanced(),
        FieldSchema::new(
            "min_interval_secs",
            "Minimum seconds between queries",
            FieldKind::Number,
        )
        .default_value(0)
        .advanced(),
    ];
    if protocol == Protocol::Torrent {
        v.extend([
            FieldSchema::new("minimum_seeders", "Minimum seeders", FieldKind::Number)
                .default_value(1)
                .advanced(),
            FieldSchema::new("seed_ratio", "Seed ratio", FieldKind::Number)
                .help("Ratio to reach before a torrent may be removed; empty uses the client default")
                .advanced(),
            FieldSchema::new("seed_time_minutes", "Seed time (minutes)", FieldKind::Number)
                .help("Empty uses the client default")
                .advanced(),
        ]);
    }
    v
}

/// Protocol of an indexer implementation.
pub fn indexer_protocol(implementation: &str, defs: &DefinitionStore) -> Option<Protocol> {
    match implementation {
        "torznab" => Some(Protocol::Torrent),
        "newznab" => Some(Protocol::Usenet),
        reelix_indexers::slskd::IMPLEMENTATION => Some(Protocol::Soulseek),
        other => {
            let def = defs.get(other.strip_prefix(IMPLEMENTATION_PREFIX)?)?;
            Some(match def.protocol.as_deref() {
                Some(p) if p.eq_ignore_ascii_case("usenet") => Protocol::Usenet,
                _ => Protocol::Torrent,
            })
        }
    }
}

/// Every schema (with catalog facets) for `kind`.
pub fn catalog(kind: ProviderKind, defs: &DefinitionStore) -> Vec<ProviderSchemaDto> {
    match kind {
        ProviderKind::Indexer => {
            let builtin = reelix_indexers::schemas();
            let mut out: Vec<ProviderSchemaDto> = builtin
                .into_iter()
                .map(|mut s| {
                    let protocol = indexer_protocol(&s.implementation, defs).unwrap_or(Protocol::Torrent);
                    s.fields.extend(indexer_fields(protocol));
                    ProviderSchemaDto {
                        schema: s,
                        protocol: Some(protocol),
                        privacy: None,
                        language: None,
                        source: "builtin".into(),
                    }
                })
                .collect();
            for def in defs.list() {
                let mut s = reelix_indexers::definition::schema(&def);
                let protocol = indexer_protocol(&s.implementation, defs).unwrap_or(Protocol::Torrent);
                s.fields.extend(indexer_fields(protocol));
                out.push(ProviderSchemaDto {
                    schema: s,
                    protocol: Some(protocol),
                    privacy: Some(def.site_type.clone()).filter(|t| !t.is_empty()),
                    language: Some(def.language.clone()).filter(|t| !t.is_empty()),
                    source: "definition".into(),
                });
            }
            out
        }
        ProviderKind::DownloadClient => reelix_downloaders::schemas()
            .into_iter()
            .map(|mut s| {
                s.fields.retain(|f| !ROW_FIELDS.contains(&f.name.as_str()));
                let protocol = reelix_downloaders::protocol_of(&s.implementation);
                ProviderSchemaDto {
                    schema: s,
                    protocol,
                    privacy: None,
                    language: None,
                    source: "builtin".into(),
                }
            })
            .collect(),
        ProviderKind::Notification => plain(reelix_notify::schemas()),
        ProviderKind::MediaServer => plain(reelix_mediaserver::schemas()),
        ProviderKind::ImportList => plain(
            reelix_lists::schemas()
                .into_iter()
                .map(|mut s| {
                    match reelix_lists::content_of(&s.implementation) {
                        reelix_lists::ListContent::Music => s.fields.extend(music_import_list_fields()),
                        reelix_lists::ListContent::Books => s.fields.extend(book_import_list_fields()),
                        reelix_lists::ListContent::Video => s.fields.extend(import_list_fields()),
                    }
                    s
                })
                .collect(),
        ),
    }
}

fn options(values: &[(&str, &str)]) -> Vec<SelectOption> {
    values
        .iter()
        .map(|(v, l)| SelectOption {
            value: (*v).into(),
            label: (*l).into(),
        })
        .collect()
}

/// `video`, `music` or `books`: what an import-list implementation yields (and so which target fields
/// its schema carries).
pub fn import_list_content(implementation: &str) -> &'static str {
    reelix_lists::content_of(implementation).as_str()
}

/// `(implementation, content)` of every import-list implementation.
pub fn import_list_contents() -> Vec<(String, &'static str)> {
    reelix_lists::schemas()
        .into_iter()
        .map(|s| {
            let c = import_list_content(&s.implementation);
            (s.implementation, c)
        })
        .collect()
}

/// Names of the Reelix-level fields added to every music import-list schema.
pub const MUSIC_IMPORT_LIST_FIELDS: [&str; 8] = [
    "enabled_auto_add",
    "quality_profile_id",
    "metadata_profile_id",
    "root_folder_id",
    "artist_monitor",
    "monitor_new_items",
    "album_handling",
    "search_on_add",
];

/// How artists / albums of a music list are added (the row's tags are applied to added artists).
fn music_import_list_fields() -> Vec<FieldSchema> {
    vec![
        FieldSchema::new("enabled_auto_add", "Add automatically", FieldKind::Bool)
            .default_value(true)
            .help("Add new list artists / albums to the library on every sync"),
        FieldSchema::new("quality_profile_id", "Quality profile", FieldKind::Number)
            .help("Music quality profile id; empty uses the first music profile"),
        FieldSchema::new("metadata_profile_id", "Metadata profile", FieldKind::Number)
            .help("Metadata profile id; empty uses the first metadata profile"),
        FieldSchema::new("root_folder_id", "Root folder", FieldKind::Number)
            .help("Music root folder id; empty uses the first music root folder"),
        FieldSchema::new(
            "artist_monitor",
            "Monitor (artists)",
            FieldKind::Select {
                options: options(&[
                    ("all", "All albums"),
                    ("future", "Future albums"),
                    ("missing", "Missing albums"),
                    ("existing", "Existing albums"),
                    ("latest", "Latest album"),
                    ("first", "First album"),
                    ("none", "None"),
                ]),
            },
        )
        .default_value("all"),
        FieldSchema::new(
            "monitor_new_items",
            "Monitor new albums",
            FieldKind::Select {
                options: options(&[("all", "All"), ("new", "Released after adding"), ("none", "None")]),
            },
        )
        .default_value("all"),
        FieldSchema::new(
            "album_handling",
            "List albums",
            FieldKind::Select {
                options: options(&[
                    ("album", "Add only the album"),
                    ("artist", "Add the album's artist"),
                ]),
            },
        )
        .default_value("album")
        .help("What to add for album entries (artist entries always add the artist)"),
        FieldSchema::new("search_on_add", "Search on add", FieldKind::Bool)
            .default_value(true)
            .help("Search for monitored albums right away"),
    ]
}

/// Names of the Reelix-level fields added to every book import-list schema (RFX-88).
pub const BOOK_IMPORT_LIST_FIELDS: [&str; 7] = [
    "enabled_auto_add",
    "formats",
    "quality_profile_id",
    "root_folder_id",
    "audio_quality_profile_id",
    "audio_root_folder_id",
    "search_on_add",
];

/// How the books of a list are added (the row's tags are applied to added authors).
fn book_import_list_fields() -> Vec<FieldSchema> {
    vec![
        FieldSchema::new("enabled_auto_add", "Add automatically", FieldKind::Bool)
            .default_value(true)
            .help("Add new books from the list on every sync"),
        FieldSchema::new(
            "formats",
            "Formats",
            FieldKind::Select {
                options: options(&[
                    ("both", "Ebook and audiobook"),
                    ("ebook", "Ebook only"),
                    ("audiobook", "Audiobook only"),
                ]),
            },
        )
        .default_value("both"),
        FieldSchema::new("quality_profile_id", "Ebook quality profile", FieldKind::Number)
            .help("Ebook quality profile id; empty uses the first ebook profile"),
        FieldSchema::new("root_folder_id", "Ebook root folder", FieldKind::Number)
            .help("Ebook root folder id; empty uses the first ebook root folder"),
        FieldSchema::new(
            "audio_quality_profile_id",
            "Audiobook quality profile",
            FieldKind::Number,
        )
        .help("Audiobook quality profile id; empty uses the first audiobook profile"),
        FieldSchema::new("audio_root_folder_id", "Audiobook root folder", FieldKind::Number)
            .help("Audiobook root folder id; empty uses the first audiobook root folder"),
        FieldSchema::new("search_on_add", "Search on add", FieldKind::Bool)
            .default_value(true)
            .help("Search for added books right away"),
    ]
}

/// Names of the Reelix-level fields added to every movie/series import-list schema.
pub const IMPORT_LIST_FIELDS: [&str; 9] = [
    "enabled_auto_add",
    "quality_profile_id",
    "root_folder_id",
    "monitor",
    "series_monitor",
    "minimum_availability",
    "series_type",
    "season_folder",
    "search_on_add",
];

/// How items of a list are added to the library (the row's tags are applied to added media).
fn import_list_fields() -> Vec<FieldSchema> {
    vec![
        FieldSchema::new("enabled_auto_add", "Add automatically", FieldKind::Bool)
            .default_value(true)
            .help("Add new list items to the library on every sync"),
        FieldSchema::new("quality_profile_id", "Quality profile", FieldKind::Number)
            .help("Quality profile id; empty uses the first profile"),
        FieldSchema::new("root_folder_id", "Root folder", FieldKind::Number).help(
            "Root folder id; empty (or a folder of the other media type) uses the first matching root folder",
        ),
        FieldSchema::new(
            "monitor",
            "Monitor (movies)",
            FieldKind::Select {
                options: options(&[
                    ("movie_only", "Movie only"),
                    ("movie_and_collection", "Movie and collection"),
                    ("none", "None"),
                ]),
            },
        )
        .default_value("movie_only"),
        FieldSchema::new(
            "series_monitor",
            "Monitor (series)",
            FieldKind::Select {
                options: options(&[
                    ("all", "All episodes"),
                    ("future", "Future episodes"),
                    ("missing", "Missing episodes"),
                    ("existing", "Existing episodes"),
                    ("first_season", "First season"),
                    ("latest_season", "Latest season"),
                    ("pilot", "Pilot episode"),
                    ("none", "None"),
                ]),
            },
        )
        .default_value("all"),
        FieldSchema::new(
            "minimum_availability",
            "Minimum availability",
            FieldKind::Select {
                options: options(&[
                    ("announced", "Announced"),
                    ("in_cinemas", "In cinemas"),
                    ("released", "Released"),
                ]),
            },
        )
        .default_value("released"),
        FieldSchema::new(
            "series_type",
            "Series type",
            FieldKind::Select {
                options: options(&[("standard", "Standard"), ("daily", "Daily"), ("anime", "Anime")]),
            },
        )
        .default_value("standard"),
        FieldSchema::new("season_folder", "Season folders", FieldKind::Bool).default_value(true),
        FieldSchema::new("search_on_add", "Search on add", FieldKind::Bool)
            .default_value(true)
            .help("Search for added items right away"),
    ]
}

fn plain(schemas: Vec<ProviderSchema>) -> Vec<ProviderSchemaDto> {
    schemas
        .into_iter()
        .map(|s| ProviderSchemaDto {
            schema: s,
            protocol: None,
            privacy: None,
            language: None,
            source: "builtin".into(),
        })
        .collect()
}

/// The (augmented) schema of one implementation.
pub fn find(kind: ProviderKind, implementation: &str, defs: &DefinitionStore) -> Option<ProviderSchemaDto> {
    if kind == ProviderKind::Indexer {
        // Avoid building every definition schema for a single lookup.
        let protocol = indexer_protocol(implementation, defs)?;
        let mut s = match implementation {
            "torznab" | "newznab" | reelix_indexers::slskd::IMPLEMENTATION => reelix_indexers::schemas()
                .into_iter()
                .find(|s| s.implementation == implementation)?,
            other => {
                let def = defs.get(other.strip_prefix(IMPLEMENTATION_PREFIX)?)?;
                reelix_indexers::definition::schema(&def)
            }
        };
        s.fields.extend(indexer_fields(protocol));
        return Some(ProviderSchemaDto {
            schema: s,
            protocol: Some(protocol),
            privacy: None,
            language: None,
            source: if implementation.starts_with(IMPLEMENTATION_PREFIX) {
                "definition".into()
            } else {
                "builtin".into()
            },
        });
    }
    catalog(kind, defs)
        .into_iter()
        .find(|s| s.schema.implementation == implementation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_are_augmented() {
        let defs = DefinitionStore::default();
        let idx = catalog(ProviderKind::Indexer, &defs);
        assert_eq!(idx.len(), 3);
        let torznab = idx.iter().find(|s| s.schema.implementation == "torznab").unwrap();
        assert_eq!(torznab.protocol, Some(Protocol::Torrent));
        assert!(torznab.schema.fields.iter().any(|f| f.name == "seed_ratio"));
        let newznab = find(ProviderKind::Indexer, "newznab", &defs).unwrap();
        assert!(!newznab.schema.fields.iter().any(|f| f.name == "seed_ratio"));
        assert!(newznab.schema.fields.iter().any(|f| f.name == "enable_rss"));
        let slskd = find(ProviderKind::Indexer, "slskd", &defs).unwrap();
        assert_eq!(slskd.protocol, Some(Protocol::Soulseek));
        let rss = slskd
            .schema
            .fields
            .iter()
            .find(|f| f.name == "enable_rss")
            .unwrap();
        assert_eq!(rss.default, Some(serde_json::Value::Bool(false)));
        assert!(!slskd.schema.fields.iter().any(|f| f.name == "minimum_seeders"));
        let client = find(ProviderKind::DownloadClient, "slskd", &defs).unwrap();
        assert_eq!(client.protocol, Some(Protocol::Soulseek));

        let dc = catalog(ProviderKind::DownloadClient, &defs);
        assert!(
            dc.iter()
                .all(|s| !s.schema.fields.iter().any(|f| f.name == "name"))
        );
        assert!(dc.iter().all(|s| s.protocol.is_some()));
        for k in ProviderKind::ALL {
            assert!(!catalog(k, &defs).is_empty(), "{k:?}");
        }
        assert!(find(ProviderKind::Notification, "discord", &defs).is_some());
        for s in catalog(ProviderKind::ImportList, &defs) {
            let fields: &[&str] = match import_list_content(&s.schema.implementation) {
                "music" => &MUSIC_IMPORT_LIST_FIELDS,
                "books" => &BOOK_IMPORT_LIST_FIELDS,
                _ => &IMPORT_LIST_FIELDS,
            };
            for f in fields {
                assert!(
                    s.schema.fields.iter().any(|x| x.name == *f),
                    "{} lacks {f}",
                    s.schema.implementation
                );
            }
        }
        assert!(find(ProviderKind::Indexer, "definition:nope", &defs).is_none());
    }
}
