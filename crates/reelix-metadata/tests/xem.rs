//! TheXEM client against a mocked API with authored fixture payloads.

use reelix_metadata::{SceneName, SceneNumber, Xem, XemOptions};
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok(name: &str) -> ResponseTemplate {
    let p = format!("{}/tests/fixtures/xem/{name}", env!("CARGO_MANIFEST_DIR"));
    ResponseTemplate::new(200).set_body_string(std::fs::read_to_string(p).unwrap())
}

fn client(server: &MockServer) -> Xem {
    Xem::new(
        reqwest::Client::new(),
        XemOptions {
            base_url: server.uri(),
            ..Default::default()
        },
    )
}

fn num(season: u32, episode: u32, absolute: Option<u32>) -> SceneNumber {
    SceneNumber {
        season,
        episode,
        absolute,
    }
}

#[tokio::test]
async fn mappings_translate_both_ways() {
    let server = MockServer::start().await;
    Mock::given(path("/map/all"))
        .and(query_param("id", "267440"))
        .and(query_param("origin", "tvdb"))
        .respond_with(ok("map_all_267440.json"))
        .expect(1)
        .mount(&server)
        .await;
    let xem = client(&server);
    let m = xem.mappings(267440).await.unwrap();
    assert_eq!(m.entries.len(), 8);
    assert_eq!(m.scene_seasons(), [1, 2, 3, 4]);
    assert_eq!(
        m.tvdb_for_scene(4, 29),
        Some(num(0, 13, None)),
        "scene S04E29 is a TVDB special"
    );
    assert_eq!(m.scene_for(0, 13), Some(num(4, 29, Some(88))));
    assert_eq!(m.tvdb_for_scene_absolute(60), Some(num(4, 1, Some(60))));
    assert_eq!(m.tvdb_for_scene(9, 9), None);
    // Cached.
    assert_eq!(xem.mappings(267440).await.unwrap(), m);
}

#[tokio::test]
async fn failure_result_is_empty_mapping() {
    let server = MockServer::start().await;
    Mock::given(path("/map/all"))
        .respond_with(ok("map_failure.json"))
        .mount(&server)
        .await;
    let m = client(&server).mappings(999999).await.unwrap();
    assert!(m.is_empty());
    assert_eq!(m.tvdb_id, 999999);
}

#[tokio::test]
async fn havemap_accepts_string_and_numeric_ids() {
    let server = MockServer::start().await;
    Mock::given(path("/map/havemap"))
        .respond_with(ok("havemap.json"))
        .mount(&server)
        .await;
    let ids = client(&server).shows_with_mappings().await.unwrap();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&267440) && ids.contains(&81797));
}

#[tokio::test]
async fn scene_names_with_seasons() {
    let server = MockServer::start().await;
    Mock::given(path("/map/allNames"))
        .and(query_param("seasonNumbers", "1"))
        .respond_with(ok("all_names.json"))
        .expect(1)
        .mount(&server)
        .await;
    let xem = client(&server);
    let names = xem.names(267440).await.unwrap();
    assert_eq!(
        names,
        [
            SceneName {
                name: "Shingeki no Kyojin".into(),
                season: None
            },
            SceneName {
                name: "Attack on Titan Final Season".into(),
                season: Some(4)
            },
            SceneName {
                name: "SnK".into(),
                season: None
            },
        ]
    );
    let by_season = xem.names_by_season(79824).await.unwrap();
    assert_eq!(by_season[&None], ["Naruto Shippuuden", "Naruto Shippuden"]);
    assert!(xem.names(1).await.unwrap().is_empty());
}
