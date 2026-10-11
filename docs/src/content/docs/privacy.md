---
title: Privacy Policy
description: What ytkew keeps on your machine, what it sends, and where it goes.
---

ytkew is a local terminal application. It has no server, no user accounts, and
no analytics. Almost everything it touches stays on your computer. The only
things that leave are the requests needed to talk to YouTube Music and — if you
turn it on — Discord.

## What it stores

ytkew keeps its files in `~/.config/ytkew/`:

- `cookie.txt` — your YouTube Music session cookie, if you signed in with
  `--auth cookie` or `--auth browser`.
- `oauth.json` — the OAuth access and refresh tokens, plus the client id and
  secret of the Google Cloud OAuth client you created, if you signed in with
  `--auth oauth`.
- `config.toml` — your settings.
- `state.toml` — playback state: volume, shuffle and repeat, cover size, theme,
  and the Discord toggles.
- `ytkew.log` — a local log file.

Cover art is cached under your user cache directory (on Linux,
`~/.cache/ytkew/`).

The two credential files are written with owner-only permissions (`0600`) and
are read by nothing but ytkew.

## What leaves your computer

- **Google.** To reach your library, ytkew sends your credential to YouTube
  Music's internal API (`music.youtube.com/youtubei`) and refreshes OAuth
  tokens at `oauth2.googleapis.com`. Your credential is sent to Google and to
  nobody else; no ytkew-operated server is involved at any point.
- **Cover art.** Album and track images are fetched from Google's image hosts
  (`i.ytimg.com`, `yt3.googleusercontent.com`).
- **Discord — only if you enable Discord Rich Presence.** The current track
  (title, artist, album, playback position, and a link to this repository when
  the GitHub button is on) is sent to the Discord desktop application running
  on your machine over its local IPC socket; Discord then shares it according
  to your Discord settings. To show the cover there, the image URL is passed
  through `images.weserv.nl`, a third-party image-resizing service, before
  Discord fetches it.
- **Nothing is sent to the ytkew developer.** There is no telemetry, no
  analytics, no crash reporting, and no advertising.

Search and playback run locally: `yt-dlp` resolves streams from YouTube and
`mpv` plays them.

## Cookies and OAuth

Signing in is optional. Search and playback work without it; only your library
needs credentials. With `--auth browser` or `--auth cookie`, ytkew uses your
existing YouTube Music session, which authenticates as you. With `--auth
oauth`, it uses a token issued by a Google Cloud OAuth client you control (type
"TVs and Limited Input devices"). Treat `cookie.txt` and `oauth.json` like
passwords.

## Deleting your data

Everything ytkew stores is local and yours to remove: delete `~/.config/ytkew/`
(and the cover cache) and it is gone. To invalidate the credential on Google's
side as well, sign out of Google everywhere or revoke ytkew's access in your
[Google Account permissions](https://myaccount.google.com/permissions).

## Third parties

- Google / YouTube Music — [privacy policy](https://policies.google.com/privacy)
- Discord — [privacy policy](https://discord.com/privacy)
- images.weserv.nl — [website](https://images.weserv.nl/)

## Changes and contact

This policy may change as the program does. Questions can be raised as an issue
at [github.com/noxygalaxy/ytkew-fixed](https://github.com/noxygalaxy/ytkew-fixed).

_Last updated: 11 October 2026._
