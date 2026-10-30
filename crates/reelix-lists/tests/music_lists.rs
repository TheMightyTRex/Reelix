//! Music list adapters (Last.fm, ListenBrainz, Spotify) against wiremock (no real network).

use reelix_lists::{
    BuildContext, ImportList, ListContent, ListError, MusicItemKind, MusicListItem, build, content_of,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const BJORK: &str = "87c5dedd-371d-4a53-9f7f-80522fb7f3cb";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";

async fn setup() -> (MockServer, BuildContext) {
    let server = MockServer::start().await;
    let ctx = BuildContext::new(reqwest::Client::new()).with_all_base_urls(&server.uri());
    (server, ctx)
}

fn list(ctx: &BuildContext, imp: &str, settings: Value) -> Box<dyn ImportList> {
    build(imp, &settings, ctx).unwrap_or_else(|e| panic!("build {imp}: {e}"))
}

type Row<'a> = (
    MusicItemKind,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

fn rows(items: &[MusicListItem]) -> Vec<Row<'_>> {
    items
        .iter()
        .map(|i| {
            (
                i.kind,
                i.artist_name.as_str(),
                i.artist_mbid.as_deref(),
                i.album_title.as_deref(),
                i.album_mbid.as_deref(),
            )
        })
        .collect()
}

#[test]
fn music_lists_declare_music_content() {
    let ctx = BuildContext::new(reqwest::Client::new());
    for (imp, s) in [
        ("lastfm", json!({"api_key": "k", "username": "u"})),
        ("listenbrainz", json!({"username": "u"})),
        (
            "spotify",
            json!({"client_id": "c", "client_secret": "s", "playlist_id": "abc123"}),
        ),
    ] {
        assert_eq!(content_of(imp), ListContent::Music);
        assert_eq!(list(&ctx, imp, s).content(), ListContent::Music, "{imp}");
    }
    assert_eq!(content_of("tmdb"), ListContent::Video);
}

#[test]
fn invalid_music_settings_are_rejected() {
    let ctx = BuildContext::new(reqwest::Client::new());
    for (imp, s) in [
        ("lastfm", json!({"username": "u"})),
        (
            "lastfm",
            json!({"api_key": "k", "username": "u", "period": "decade"}),
        ),
        ("listenbrainz", json!({"list_type": "top_artists"})),
        (
            "listenbrainz",
            json!({"list_type": "playlist_artists", "playlist_id": "nope"}),
        ),
        ("spotify", json!({"client_id": "c", "client_secret": "s"})),
        (
            "spotify",
            json!({"client_id": "c", "client_secret": "s", "list_type": "saved_albums"}),
        ),
    ] {
        assert!(
            matches!(build(imp, &s, &ctx), Err(ListError::InvalidSettings(_))),
            "{imp} {s}"
        );
    }
}

#[tokio::test]
async fn lastfm_top_artists_pages_and_keeps_mbids() {
    let (server, ctx) = setup().await;
    let page = |n: &str, artists: Value| json!({"topartists": {"artist": artists, "@attr": {"page": n, "totalPages": "2", "user": "bob"}}});
    Mock::given(method("GET"))
        .and(path("/2.0/"))
        .and(query_param("method", "user.gettopartists"))
        .and(query_param("api_key", "key"))
        .and(query_param("period", "12month"))
        .and(query_param("limit", "2"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            "1",
            json!([{"name": "Radiohead", "mbid": RADIOHEAD}, {"name": "Björk", "mbid": ""}]),
        )))
        .expect(1)
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "lastfm",
        json!({"api_key": "key", "username": "bob", "period": "12month", "limit": 2}),
    );
    let items = l.fetch_music().await.unwrap();
    assert_eq!(
        rows(&items),
        [
            (MusicItemKind::Artist, "Radiohead", Some(RADIOHEAD), None, None),
            (MusicItemKind::Artist, "Björk", None, None, None),
        ]
    );
    assert!(l.fetch().await.unwrap().is_empty());
}

#[tokio::test]
async fn lastfm_top_albums_and_loved_tracks() {
    let (server, ctx) = setup().await;
    Mock::given(path("/2.0/"))
        .and(query_param("method", "user.gettopalbums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"topalbums": {
            "album": [{"name": "OK Computer", "mbid": "0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29",
                       "artist": {"name": "Radiohead", "mbid": RADIOHEAD}}],
            "@attr": {"page": "1", "totalPages": "1"}}})))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/"))
        .and(query_param("method", "user.getlovedtracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"lovedtracks": {
            "track": [
                {"name": "Hyperballad", "artist": {"name": "Björk", "mbid": BJORK}},
                {"name": "Jóga", "artist": {"name": "Björk", "mbid": BJORK}},
                {"name": "Airbag", "artist": {"name": "Radiohead", "mbid": RADIOHEAD}}
            ],
            "@attr": {"page": "1", "totalPages": "1"}}})))
        .mount(&server)
        .await;
    let albums = list(
        &ctx,
        "lastfm",
        json!({"api_key": "k", "username": "bob", "list_type": "top_albums"}),
    )
    .fetch_music()
    .await
    .unwrap();
    // Last.fm album mbids are release ids, not release groups: dropped.
    assert_eq!(
        rows(&albums),
        [(
            MusicItemKind::Album,
            "Radiohead",
            Some(RADIOHEAD),
            Some("OK Computer"),
            None
        )]
    );
    let loved = list(
        &ctx,
        "lastfm",
        json!({"api_key": "k", "username": "bob", "list_type": "loved_tracks"}),
    )
    .fetch_music()
    .await
    .unwrap();
    assert_eq!(
        rows(&loved),
        [
            (MusicItemKind::Artist, "Björk", Some(BJORK), None, None),
            (MusicItemKind::Artist, "Radiohead", Some(RADIOHEAD), None, None),
        ]
    );
}

#[tokio::test]
async fn lastfm_invalid_key_is_unauthorized() {
    let (server, ctx) = setup().await;
    Mock::given(path("/2.0/"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({"error": 10, "message": "Invalid API key"})),
        )
        .mount(&server)
        .await;
    let l = list(&ctx, "lastfm", json!({"api_key": "bad", "username": "bob"}));
    assert!(matches!(l.test().await, Err(ListError::Unauthorized)));
}

#[tokio::test]
async fn listenbrainz_stats_and_token() {
    let (server, ctx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/1/stats/user/alice/artists"))
        .and(query_param("range", "month"))
        .and(header("authorization", "Token tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"payload": {
            "artists": [
                {"artist_mbid": RADIOHEAD, "artist_name": "Radiohead", "listen_count": 40},
                {"artist_mbid": null, "artist_mbids": [BJORK], "artist_name": "Björk", "listen_count": 12}
            ], "count": 2, "range": "month", "user_id": "alice"}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/1/stats/user/alice/release-groups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"payload": {
            "release_groups": [{"release_group_mbid": OK_COMPUTER, "release_group_name": "OK Computer",
                                "artist_name": "Radiohead", "artist_mbids": [RADIOHEAD], "listen_count": 9}]}})))
        .mount(&server)
        .await;
    Mock::given(path("/1/stats/user/newbie/artists"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let artists = list(
        &ctx,
        "listenbrainz",
        json!({"username": "alice", "range": "month", "token": "tok"}),
    )
    .fetch_music()
    .await
    .unwrap();
    assert_eq!(
        rows(&artists),
        [
            (MusicItemKind::Artist, "Radiohead", Some(RADIOHEAD), None, None),
            (MusicItemKind::Artist, "Björk", Some(BJORK), None, None),
        ]
    );
    let albums = list(
        &ctx,
        "listenbrainz",
        json!({"username": "alice", "list_type": "top_release_groups"}),
    )
    .fetch_music()
    .await
    .unwrap();
    assert_eq!(
        rows(&albums),
        [(
            MusicItemKind::Album,
            "Radiohead",
            Some(RADIOHEAD),
            Some("OK Computer"),
            Some(OK_COMPUTER)
        )]
    );
    let none = list(&ctx, "listenbrainz", json!({"username": "newbie"}));
    assert!(none.fetch_music().await.unwrap().is_empty());
}

#[tokio::test]
async fn listenbrainz_playlist_artists() {
    let (server, ctx) = setup().await;
    let pid = "11111111-2222-3333-4444-555555555555";
    let ext = |artist: &str| {
        json!({"https://musicbrainz.org/doc/jspf#track": {
            "artist_identifiers": [format!("https://musicbrainz.org/artist/{artist}")]}})
    };
    Mock::given(path(format!("/1/playlist/{pid}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"playlist": {"title": "Weekly", "track": [
            {"title": "Airbag", "creator": "Radiohead", "album": "OK Computer", "extension": ext(RADIOHEAD)},
            {"title": "Karma Police", "creator": "Radiohead", "album": "OK Computer", "extension": ext(RADIOHEAD)},
            {"title": "Army of Me", "creator": "Björk", "album": "Post"}
        ]}})))
        .mount(&server)
        .await;
    let url = format!("https://listenbrainz.org/playlist/{pid}");
    let artists = list(
        &ctx,
        "listenbrainz",
        json!({"list_type": "playlist_artists", "playlist_id": url}),
    )
    .fetch_music()
    .await
    .unwrap();
    assert_eq!(
        rows(&artists),
        [
            (MusicItemKind::Artist, "Radiohead", Some(RADIOHEAD), None, None),
            (MusicItemKind::Artist, "Björk", None, None, None),
        ]
    );
    let albums = list(
        &ctx,
        "listenbrainz",
        json!({"list_type": "playlist_albums", "playlist_id": pid}),
    )
    .fetch_music()
    .await
    .unwrap();
    assert_eq!(albums.len(), 2);
    assert_eq!(albums[1].album_title.as_deref(), Some("Post"));
}

#[tokio::test]
async fn spotify_playlist_with_client_credentials() {
    let (server, ctx) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .and(header("authorization", "Basic Y2lkOnNlY3JldA=="))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "token_type": "Bearer", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let track = |artist: &str, album: &str, date: &str| {
        json!({"track": {"type": "track", "name": "t", "artists": [{"name": artist}],
            "album": {"name": album, "release_date": date, "artists": [{"name": artist}]}}})
    };
    Mock::given(method("GET"))
        .and(path("/v1/playlists/PL1/tracks"))
        .and(header("authorization", "Bearer at"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                track("Radiohead", "OK Computer", "1997-05-21"),
                track("Radiohead", "Kid A", "2000-10-02"),
                {"track": {"is_local": true, "artists": [{"name": "Local"}]}},
                track("Björk", "Post", "1995")
            ],
            "total": 4, "next": null})))
        .expect(1)
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "spotify",
        json!({"client_id": "cid", "client_secret": "secret",
               "playlist_id": "https://open.spotify.com/playlist/PL1?si=x"}),
    );
    assert_eq!(
        rows(&l.fetch_music().await.unwrap()),
        [
            (MusicItemKind::Artist, "Radiohead", None, None, None),
            (MusicItemKind::Artist, "Björk", None, None, None),
        ]
    );
}

#[tokio::test]
async fn spotify_user_library_uses_refresh_token() {
    let (server, ctx) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": "ut"})))
        .mount(&server)
        .await;
    Mock::given(path("/v1/me/albums"))
        .and(header("authorization", "Bearer ut"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"added_at": "2024-01-01T00:00:00Z", "album": {"name": "OK Computer",
                "release_date": "1997-05-21", "artists": [{"name": "Radiohead"}],
                "external_ids": {"upc": "724385522925"}}}],
            "total": 1})))
        .mount(&server)
        .await;
    Mock::given(path("/v1/me/following"))
        .and(query_param("type", "artist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"artists": {
            "items": [{"id": "1", "name": "Radiohead"}, {"id": "2", "name": "Björk"}],
            "cursors": {"after": null}, "total": 2}})))
        .mount(&server)
        .await;
    let base = json!({"client_id": "c", "client_secret": "s", "refresh_token": "rt"});
    let with = |t: &str| {
        let mut s = base.clone();
        s["list_type"] = t.into();
        s
    };
    let albums = list(&ctx, "spotify", with("saved_albums"))
        .fetch_music()
        .await
        .unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].kind, MusicItemKind::Album);
    assert_eq!(albums[0].album_title.as_deref(), Some("OK Computer"));
    assert_eq!(albums[0].year, Some(1997));
    assert_eq!(albums[0].upc.as_deref(), Some("724385522925"));
    let artists = list(&ctx, "spotify", with("followed_artists"))
        .fetch_music()
        .await
        .unwrap();
    assert_eq!(
        artists.iter().map(|a| a.artist_name.as_str()).collect::<Vec<_>>(),
        ["Radiohead", "Björk"]
    );
}

#[tokio::test]
async fn spotify_bad_client_is_unauthorized() {
    let (server, ctx) = setup().await;
    Mock::given(path("/api/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_client"})))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "spotify",
        json!({"client_id": "c", "client_secret": "bad", "playlist_id": "PL1"}),
    );
    assert!(matches!(l.test().await, Err(ListError::Unauthorized)));
}
