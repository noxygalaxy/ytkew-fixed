use crate::discord::{bar_free, square_cover, Button, Discord, Presence, GITHUB_URL, LOGO};
use super::*;

fn anchor_track(anchor: &mut Option<(String, i64)>, video_id: &str, elapsed: f64) {
    if video_id.is_empty() {
        *anchor = None;
        return;
    }
    if anchor.as_ref().is_some_and(|(prev, _)| prev == video_id) {
        return;
    }
    *anchor = Some((
        video_id.to_string(),
        Discord::now() - elapsed.max(0.0) as i64,
    ));
}

impl App {
    pub fn set_presence(&mut self, on: bool) {
        if self.discord_enabled == on {
            return;
        }
        self.discord_enabled = on;
        self.discord.set_enabled(on);
    }
    pub fn presence(&self) -> Presence {
        let track = self.queue.current();
        let show_track = self.discord_song && track.is_some();

        let (details, state, large_image, large_text, window) = match track {
            Some(t) if show_track => {
                let window = if self.player_state.paused {
                    None
                } else {
                    self.discord_anchor.as_ref().and_then(|(_, start)| {
                        t.duration
                            .filter(|d| *d > 0.0)
                            .map(|d| (*start, *start + d as i64))
                    })
                };
                (
                    Some(t.title.clone()),
                    Some(if t.artist.is_empty() {
                        t.album.clone().unwrap_or_else(|| "ytkew".into())
                    } else {
                        t.artist.clone()
                    }),
                    t.cover_url().map(|u| square_cover(&bar_free(&u))),
                    Some(t.title.clone()),
                    window,
                )
            }
            Some(_) => (
                Some("song details are off".into()),
                Some("ytkew".into()),
                None,
                None,
                None,
            ),
            None => (
                Some("not listening".into()),
                Some("ytkew".into()),
                None,
                None,
                None,
            ),
        };

        let mut buttons = Vec::new();
        if self.discord_github {
            buttons.push(Button {
                label: "github".into(),
                url: GITHUB_URL.into(),
            });
        }

        Presence {
            details,
            state,
            large_image: large_image.or_else(|| Some(LOGO.into())),
            large_text: large_text.or_else(|| Some(LOGO.into())),
            small_image: show_track.then(|| LOGO.into()),
            small_text: show_track.then(|| LOGO.into()),
            window,
            buttons,
        }
    }

    fn refresh_anchor(&mut self) {
        let elapsed = self.player_state.time_pos.max(0.0);
        let id = self
            .queue
            .current()
            .map(|t| t.video_id.clone())
            .unwrap_or_default();
        anchor_track(&mut self.discord_anchor, &id, elapsed);
    }

    pub async fn sync_discord(&mut self) {
        if !self.discord_enabled {
            return;
        }
        self.refresh_anchor();
        let presence = self.presence();
        self.discord.publish(&presence).await;
    }
}