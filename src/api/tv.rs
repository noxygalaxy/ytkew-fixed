use super::{AlbumRef, Playlist, MAX_ITEMS};
use crate::model::{best_thumbnail, parse_duration, Track, COVER_PX};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use ytmapi_rs::common::{LikeStatus, Thumbnail};

const API: &str = "https://music.youtube.com/youtubei/v1/";
const KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const LIBRARY: &str = "FEmusic_liked_playlists";

const TAB_PLAYLISTS: &str = "Playlists";
const TAB_SONGS: &str = "Liked songs";
const TAB_ALBUMS: &str = "Albums";
const TAB_HISTORY: &str = "Recent activity";

pub(super) const NO_ARTIST_PAGES: &str =
    "YouTube TV's library has no artist pages -- use `ytkew --auth cookie` for Artists";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Token {
    token_type: String,
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    request_time: SystemTime,
    client_id: String,
    client_secret: String,
}

impl Token {
    fn stale(&self, now: SystemTime) -> bool {
        let (Ok(issued), Ok(now)) = (
            self.request_time.duration_since(UNIX_EPOCH),
            now.duration_since(UNIX_EPOCH),
        ) else {
            return true;
        };
        now.as_secs() + 60 >= issued.as_secs() + self.expires_in
    }
}

#[derive(Deserialize)]
struct GoogleToken {
    access_token: String,
    expires_in: u64,
    token_type: String,
}

pub(super) struct Tv {
    client: reqwest::Client,
    token: RwLock<Token>,
    path: Option<PathBuf>,
}

impl Tv {
    pub(super) async fn load(path: &Path) -> Result<Self> {
        let raw = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let token: Token =
            serde_json::from_str(&raw).context("oauth.json is not a valid saved token")?;
        let tv = Self {
            client: reqwest::Client::builder()
                .user_agent(UA)
                .build()
                .context("building http client")?,
            token: RwLock::new(token),
            path: Some(path.to_path_buf()),
        };
        tv.maybe_refresh().await?;
        Ok(tv)
    }

    pub(super) async fn library_playlists(
        &self,
        mut on_page: impl FnMut(Vec<Playlist>),
    ) -> Result<()> {
        let first = self.library_tab(TAB_PLAYLISTS).await?;
        self.drain_pages(first, |tiles| {
            on_page(tiles.iter().filter_map(playlist_from_tile).collect())
        })
        .await
    }

    pub(super) async fn library_songs(&self, mut on_page: impl FnMut(Vec<Track>)) -> Result<()> {
        let first = self.library_tab(TAB_SONGS).await?;
        self.drain_pages(first, |tiles| {
            on_page(tiles.iter().filter_map(track_from_tile).collect())
        })
        .await
    }

    pub(super) async fn library_albums(
        &self,
        mut on_page: impl FnMut(Vec<AlbumRef>),
    ) -> Result<()> {
        let first = self.library_tab(TAB_ALBUMS).await?;
        self.drain_pages(first, |tiles| {
            on_page(tiles.iter().filter_map(album_from_tile).collect())
        })
        .await
    }

    pub(super) async fn playlist_tracks(
        &self,
        browse_id: &str,
        mut on_page: impl FnMut(Vec<Track>),
    ) -> Result<()> {
        let res = self
            .post(
                "browse",
                &[],
                json!({ "context": Self::context(), "browseId": browse_id }),
            )
            .await?;
        self.drain_pages(playlist_page(&res), |tiles| {
            on_page(tiles.iter().filter_map(track_from_tile).collect())
        })
        .await
    }

    pub(super) async fn history_len(&self) -> Result<usize> {
        Ok(self.library_tab(TAB_HISTORY).await?.items.len())
    }

    pub(super) async fn rate(&self, video_id: &str, status: LikeStatus) -> Result<()> {
        let endpoint = match status {
            LikeStatus::Liked => "like/like",
            LikeStatus::Disliked => "like/dislike",
            LikeStatus::Indifferent => "like/removelike",
        };
        self.post(
            endpoint,
            &[],
            json!({ "context": Self::context(), "target": { "videoId": video_id } }),
        )
        .await
        .map(|_| ())
    }

    async fn library_tab(&self, tab: &str) -> Result<Grid> {
        let page = self
            .post(
                "browse",
                &[],
                json!({ "context": Self::context(), "browseId": LIBRARY }),
            )
            .await?;
        let continuation = tab_continuation(&page, tab)
            .ok_or_else(|| anyhow!("YouTube TV's library page has no {tab:?} tab"))?;
        self.continuation(&continuation).await
    }

    async fn continuation(&self, token: &str) -> Result<Grid> {
        let res = self
            .post(
                "browse",
                &[("continuation", token)],
                json!({ "context": Self::context() }),
            )
            .await?;
        Ok(grid_from_continuation(&res))
    }

    async fn drain_pages(&self, first: Grid, mut on_page: impl FnMut(Vec<Value>)) -> Result<()> {
        let mut page = first;
        let mut taken = 0usize;
        while taken < MAX_ITEMS {
            let mut items = std::mem::take(&mut page.items);
            items.truncate(MAX_ITEMS - taken);
            taken += items.len();
            if !items.is_empty() {
                on_page(items);
            }
            let Some(next) = page.next.take() else {
                return Ok(());
            };
            match self.continuation(&next).await {
                Ok(next) => page = next,
                Err(_) if taken > 0 => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn post(&self, endpoint: &str, extra: &[(&str, &str)], body: Value) -> Result<Value> {
        let bearer = self.bearer().await?;
        let mut params: Vec<(&str, &str)> =
            vec![("alt", "json"), ("prettyPrint", "false"), ("key", KEY)];
        for (k, v) in extra {
            params.push((k, v));
        }
        let res = self
            .client
            .post(format!("{API}{endpoint}"))
            .query(&params)
            .header("Content-Type", "application/json")
            .header("Authorization", &bearer)
            .body(serde_json::to_string(&body).context("encoding the request body")?)
            .send()
            .await
            .with_context(|| format!("calling {endpoint}"))?;
        let status = res.status();
        let text = res.text().await.context("reading the response body")?;
        if !status.is_success() {
            return Err(anyhow!(
                "{endpoint} answered HTTP {status}: {}",
                snippet(&text)
            ));
        }
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("{endpoint} sent back something that is not json"))?;
        if let Some(err) = value.pointer("/error") {
            let code = err["code"].as_i64().unwrap_or_default();
            let message = err["message"].as_str().unwrap_or("no message");
            return Err(anyhow!("{endpoint} reported error {code}: {message}"));
        }
        Ok(value)
    }

    async fn bearer(&self) -> Result<String> {
        self.maybe_refresh().await?;
        let token = self.token.read().await;
        Ok(format!("{} {}", token.token_type, token.access_token))
    }

    async fn maybe_refresh(&self) -> Result<()> {
        let current = self.token.read().await.clone();
        if !current.stale(SystemTime::now()) {
            return Ok(());
        }
        let fresh = self.refresh(&current).await?;
        if let Some(path) = &self.path {
            if let Ok(json) = serde_json::to_string_pretty(&fresh) {
                let _ = tokio::fs::write(path, json).await;
            }
        }
        *self.token.write().await = fresh;
        Ok(())
    }

    async fn refresh(&self, token: &Token) -> Result<Token> {
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": token.refresh_token,
            "client_secret": token.client_secret,
            "client_id": token.client_id,
        });
        let res = self
            .client
            .post(TOKEN_URL)
            .header("User-Agent", UA)
            .body(serde_json::to_string(&body).context("encoding the refresh request")?)
            .send()
            .await
            .context("refreshing the oauth token")?;
        let status = res.status();
        let text = res.text().await.context("reading the token response")?;
        if !status.is_success() {
            return Err(anyhow!(
                "the token endpoint answered HTTP {status}: {}",
                snippet(&text)
            ));
        }
        let fresh: GoogleToken = serde_json::from_str(&text)
            .with_context(|| format!("the token endpoint said: {}", snippet(&text)))?;
        Ok(Token {
            token_type: fresh.token_type,
            access_token: fresh.access_token,
            refresh_token: token.refresh_token.clone(),
            expires_in: fresh.expires_in,
            request_time: SystemTime::now(),
            client_id: token.client_id.clone(),
            client_secret: token.client_secret.clone(),
        })
    }

    fn context() -> Value {
        let today = time::OffsetDateTime::now_utc().date();
        json!({
            "client": {
                "clientName": "TVHTML5",
                "clientVersion": format!(
                    "7.{:04}{:02}{:02}.18.00",
                    today.year(),
                    u8::from(today.month()),
                    today.day()
                ),
                "hl": "en",
            },
            "user": {},
        })
    }
}

#[derive(Default)]
struct Grid {
    items: Vec<Value>,
    next: Option<String>,
}

impl Grid {
    fn from_node(node: &Value) -> Self {
        let items = node
            .get("items")
            .or_else(|| node.get("contents"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.get("tileRenderer"))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Self {
            items,
            next: next_continuation(node),
        }
    }
}

fn grid_from_continuation(res: &Value) -> Grid {
    let contents = &res["continuationContents"];
    if let Some(surface) = contents.get("tvSurfaceContentContinuation") {
        return Grid::from_node(&surface["content"]["gridRenderer"]);
    }
    match contents.as_object().and_then(|c| c.values().next()) {
        Some(node) => Grid::from_node(node),
        None => Grid::default(),
    }
}

fn playlist_page(res: &Value) -> Grid {
    Grid::from_node(
        &res["contents"]["tvBrowseRenderer"]["content"]["tvSurfaceContentRenderer"]["content"]
            ["twoColumnRenderer"]["rightColumn"]["playlistVideoListRenderer"],
    )
}

fn next_continuation(node: &Value) -> Option<String> {
    node["continuations"]
        .as_array()?
        .iter()
        .find_map(|c| c["nextContinuationData"]["continuation"].as_str())
        .map(str::to_string)
}

fn tab_continuation(page: &Value, tab: &str) -> Option<String> {
    let nav = &page["contents"]["tvBrowseRenderer"]["content"]["tvSecondaryNavRenderer"];
    nav["sections"].as_array()?.iter().find_map(|section| {
        section["tvSecondaryNavSectionRenderer"]["tabs"]
            .as_array()?
            .iter()
            .find(|t| t["tabRenderer"]["title"].as_str() == Some(tab))
            .and_then(|t| {
                t["tabRenderer"]["content"]["tvSurfaceContentRenderer"]["continuation"]
                    ["reloadContinuationData"]["continuation"]
                    .as_str()
            })
            .map(str::to_string)
    })
}

fn text(node: &Value) -> String {
    if let Some(s) = node["simpleText"].as_str() {
        return s.to_string();
    }
    node["runs"]
        .as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect())
        .unwrap_or_default()
}

fn lines(tile: &Value) -> Vec<String> {
    tile["metadata"]["tileMetadataRenderer"]["lines"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .map(|l| {
                    l["lineRenderer"]["items"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .map(|i| text(&i["lineItemRenderer"]["text"]))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn track_from_tile(tile: &Value) -> Option<Track> {
    let video_id = tile["contentId"].as_str()?.to_string();
    let title = text(&tile["metadata"]["tileMetadataRenderer"]["title"]);
    let byline = lines(tile);
    let duration_text = duration(tile);
    Some(Track {
        video_id,
        title,
        artist: byline.first().cloned().unwrap_or_default(),
        album: None,
        duration: parse_duration(&duration_text),
        duration_text,
        thumbnail: thumbnail(tile),
    })
}

fn playlist_from_tile(tile: &Value) -> Option<Playlist> {
    let id = browse_id_of(tile)?;
    let (author, track_count) = playlist_meta(&lines(tile));
    Some(Playlist {
        id,
        title: text(&tile["metadata"]["tileMetadataRenderer"]["title"]),
        author,
        track_count,
    })
}

fn album_from_tile(tile: &Value) -> Option<AlbumRef> {
    let id = browse_id_of(tile)?;
    Some(AlbumRef {
        id,
        title: text(&tile["metadata"]["tileMetadataRenderer"]["title"]),
        year: lines(tile)
            .last()
            .and_then(|l| l.split('•').next_back())
            .map(str::trim)
            .unwrap_or_default()
            .to_string(),
    })
}

fn browse_id_of(tile: &Value) -> Option<String> {
    let title = &tile["metadata"]["tileMetadataRenderer"]["title"];
    let first_run = title["runs"].as_array()?.first()?;
    Some(
        first_run["navigationEndpoint"]["browseEndpoint"]["browseId"]
            .as_str()?
            .to_string(),
    )
}

fn playlist_meta(byline: &[String]) -> (String, String) {
    let parts: Vec<&str> = byline
        .iter()
        .flat_map(|line| line.split('•'))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let author = parts
        .iter()
        .find(|p| !is_count(p))
        .copied()
        .unwrap_or_default()
        .to_string();
    let count = parts
        .iter()
        .rev()
        .find(|p| is_count(p))
        .copied()
        .unwrap_or_default()
        .to_string();
    (author, count)
}

fn is_count(part: &str) -> bool {
    let p = part.to_ascii_lowercase();
    ["playlist", "views", "ago", "songs", "tracks", "•"]
        .iter()
        .any(|marker| p.contains(marker))
}

fn duration(tile: &Value) -> String {
    tile["header"]["tileHeaderRenderer"]["thumbnailOverlays"]
        .as_array()
        .map(|overlays| {
            overlays
                .iter()
                .map(|o| text(&o["thumbnailOverlayTimeStatusRenderer"]["text"]))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn thumbnail(tile: &Value) -> Option<String> {
    let thumbs: Vec<Thumbnail> = tile["header"]["tileHeaderRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some(Thumbnail {
                        width: t["width"].as_u64()?,
                        height: t["height"].as_u64()?,
                        url: t["url"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    best_thumbnail(&thumbs, COVER_PX)
}

fn snippet(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAVED: &str = r#"{
      "token_type": "Bearer",
      "access_token": "ya29.old",
      "refresh_token": "1//03refresh",
      "expires_in": 3599,
      "request_time": { "secs_since_epoch": 1790981290, "nanos_since_epoch": 0 },
      "client_id": "1234567890-abc.apps.googleusercontent.com",
      "client_secret": "GOCSPX-abc"
    }"#;

    #[test]
    fn a_token_saved_by_ytmapi_rs_loads_unchanged() {
        let token: Token = serde_json::from_str(SAVED).unwrap();
        assert_eq!(token.access_token, "ya29.old");
        assert_eq!(token.refresh_token, "1//03refresh");
        assert_eq!(token.expires_in, 3599);
        assert_eq!(token.token_type, "Bearer");
        assert_eq!(
            token
                .request_time
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1790981290
        );
        let json = serde_json::to_value(&token).unwrap();
        assert_eq!(json["client_secret"], "GOCSPX-abc");
        assert_eq!(json["request_time"]["secs_since_epoch"], 1790981290);
    }

    #[test]
    fn an_hour_old_access_token_counts_as_stale() {
        let token: Token = serde_json::from_str(SAVED).unwrap();
        let issued = UNIX_EPOCH + std::time::Duration::from_secs(1790981290);
        assert!(token.stale(issued + std::time::Duration::from_secs(3600)));
        assert!(token.stale(issued + std::time::Duration::from_secs(3599)));
        assert!(!token.stale(issued + std::time::Duration::from_secs(3000)));
    }

    #[test]
    fn text_is_read_from_either_spelling() {
        assert_eq!(text(&json!({ "simpleText": "Äther" })), "Äther");
        assert_eq!(
            text(&json!({ "runs": [{ "text": "any" }, { "text": "thing" }] })),
            "anything"
        );
        assert_eq!(text(&json!({})), "");
    }

    fn song_tile_body() -> Value {
        json!({
            "contentId": "aaaaaaaaaaa",
            "header": { "tileHeaderRenderer": {
                "thumbnail": { "thumbnails": [
                    { "url": "https://yt3.googleusercontent.com/a=w226-h226", "width": 226, "height": 226 },
                    { "url": "https://yt3.googleusercontent.com/a=w544-h544", "width": 544, "height": 544 }
                ]},
                "thumbnailOverlays": [
                    { "thumbnailOverlayResumePlaybackRenderer": { "percentDurationWatched": 10 } },
                    { "thumbnailOverlayTimeStatusRenderer": { "text": { "simpleText": "3:59" } } }
                ]
            }},
            "metadata": { "tileMetadataRenderer": {
                "title": { "simpleText": "Slow Motion" },
                "lines": [
                    { "lineRenderer": { "items": [
                        { "lineItemRenderer": { "text": { "runs": [{ "text": "Neon Fields" }] } } }
                    ]}},
                    { "lineRenderer": { "items": [
                        { "lineItemRenderer": { "text": { "simpleText": "97K views" } } }
                    ]}}
                ]
            }}
        })
    }

    fn song_tile() -> Value {
        json!({ "tileRenderer": song_tile_body() })
    }

    #[test]
    fn a_track_tile_becomes_a_track() {
        let track = track_from_tile(&song_tile_body()).unwrap();
        assert_eq!(track.video_id, "aaaaaaaaaaa");
        assert_eq!(track.title, "Slow Motion");
        assert_eq!(track.artist, "Neon Fields");
        assert_eq!(track.duration_text, "3:59");
        assert_eq!(track.duration, Some(239.0));
        assert_eq!(
            track.thumbnail.as_deref(),
            Some("https://yt3.googleusercontent.com/a=w544-h544"),
            "the largest artwork wins, as it does for every other source"
        );
    }

    #[test]
    fn a_tile_that_is_not_a_track_is_dropped() {
        assert!(track_from_tile(&json!({})).is_none());
        assert!(playlist_from_tile(&json!({})).is_none());
        assert!(album_from_tile(&json!({})).is_none());
    }

    fn playlist_tile(byline: &[&str]) -> Value {
        let lines: Vec<Value> = byline
            .iter()
            .map(|l| {
                json!({ "lineRenderer": { "items": [
                    { "lineItemRenderer": { "text": { "simpleText": l } } }
                ]}})
            })
            .collect();
        json!({
            "contentId": "aaaaaaaaaaa",
            "metadata": { "tileMetadataRenderer": {
                "title": { "runs": [{
                    "text": "Road Trip",
                    "navigationEndpoint": { "browseEndpoint": { "browseId": "VLPLzzzzzzzzzzzzz" } }
                }]},
                "lines": lines
            }}
        })
    }

    #[test]
    fn an_auto_playlist_byline_yields_its_author_and_length() {
        let p = playlist_from_tile(&playlist_tile(&["Auto playlist • Ada • 42 songs"])).unwrap();
        assert_eq!(p.id, "VLPLzzzzzzzzzzzzz");
        assert_eq!(p.title, "Road Trip");
        assert_eq!(p.author, "Ada");
        assert_eq!(p.track_count, "42 songs");
    }

    #[test]
    fn a_two_line_playlist_byline_drops_the_view_count() {
        let p = playlist_from_tile(&playlist_tile(&[
            "Ada",
            "Playlist • 1.4K views • 42 tracks",
        ]))
        .unwrap();
        assert_eq!(p.author, "Ada");
        assert_eq!(p.track_count, "42 tracks");
    }

    #[test]
    fn an_album_tile_keeps_the_id_the_album_endpoint_wants() {
        let tile = json!({ "metadata": { "tileMetadataRenderer": {
            "title": { "runs": [{
                "text": "Second Sun",
                "navigationEndpoint": { "browseEndpoint": { "browseId": "MPREb_zzzzzzzzzzzzz" } }
            }]},
            "lines": [
                { "lineRenderer": { "items": [
                    { "lineItemRenderer": { "text": { "simpleText": "Other" } } } ]}},
                { "lineRenderer": { "items": [
                    { "lineItemRenderer": { "text": { "simpleText": "Single • 2026" } } } ]}}
            ]
        }}});
        let a = album_from_tile(&tile).unwrap();
        assert_eq!(a.id, "MPREb_zzzzzzzzzzzzz");
        assert_eq!(a.title, "Second Sun");
        assert_eq!(a.year, "2026");
    }

    #[test]
    fn a_library_tab_is_reached_through_its_reload_token() {
        let tab = |title: &str| json!({ "tabRenderer": { "title": title } });
        let with_token = json!({ "tabRenderer": {
            "title": TAB_PLAYLISTS,
            "selected": true,
            "content": nest(
                json!("Ctok-for-playlists"),
                &["tvSurfaceContentRenderer", "continuation", "reloadContinuationData", "continuation"],
            ),
        }});
        let page = nest(
            json!({ "sections": [json!({ "tvSecondaryNavSectionRenderer": {
                "tabs": [tab("Recent activity"), with_token]
            }})] }),
            &[
                "contents",
                "tvBrowseRenderer",
                "content",
                "tvSecondaryNavRenderer",
            ],
        );
        assert_eq!(
            tab_continuation(&page, TAB_PLAYLISTS).as_deref(),
            Some("Ctok-for-playlists")
        );
        assert_eq!(tab_continuation(&page, TAB_ALBUMS), None);
    }

    fn nest(value: Value, keys: &[&str]) -> Value {
        let mut out = value;
        for key in keys.iter().rev() {
            out = json!({ *key: out });
        }
        out
    }

    fn list_node(field: &str) -> Value {
        json!({
            field: [song_tile()],
            "continuations": [nest(json!("page-2"), &["nextContinuationData", "continuation"])],
        })
    }

    #[test]
    fn either_continuation_container_flattens_to_tiles() {
        let wrapped = nest(
            list_node("items"),
            &[
                "continuationContents",
                "tvSurfaceContentContinuation",
                "content",
                "gridRenderer",
            ],
        );
        let bare = nest(
            list_node("contents"),
            &["continuationContents", "playlistVideoListContinuation"],
        );
        for res in [wrapped, bare] {
            let grid = grid_from_continuation(&res);
            assert_eq!(grid.items.len(), 1);
            assert_eq!(grid.next.as_deref(), Some("page-2"));
        }
        assert_eq!(grid_from_continuation(&json!({})).items.len(), 0);
    }

    #[test]
    fn a_list_with_no_continuation_ends_after_one_page() {
        let grid = Grid::from_node(&json!({ "contents": [song_tile()] }));
        assert_eq!(grid.items.len(), 1);
        assert_eq!(grid.next, None);
    }

    #[test]
    fn an_html_error_page_is_reduced_to_its_first_line() {
        let s = snippet("<html>\n  <body>Request contains an invalid argument</body>");
        assert_eq!(
            s,
            "<html> <body>Request contains an invalid argument</body>"
        );
        assert!(snippet(&"x".repeat(500)).chars().count() <= 160);
    }
}
