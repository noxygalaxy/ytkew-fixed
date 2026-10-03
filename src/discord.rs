use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub const APP_ID: &str = "1555915561538555994";
pub const GITHUB_URL: &str = "https://github.com/noxygalaxy/ytkew-fixed";
const OPCODE_HANDSHAKE: u32 = 0;
const OPCODE_FRAME: u32 = 1;
const OPCODE_PING: u32 = 2;
const OPCODE_PONG: u32 = 3;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const COMPLAINT_PAUSE: Duration = Duration::from_secs(30);
const ACTIVITY_LISTENING: u32 = 2;
pub const LOGO: &str = "ytkew";
const RESIZE_SERVICE: &str = "https://images.weserv.nl/";

pub fn bar_free(url: &str) -> String {
    if let Some(rest) = url.split("i.ytimg.com/vi/").nth(1) {
        if let Some(id) = rest.split('/').next().filter(|s| !s.is_empty()) {
            return format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg");
        }
    }
    url.to_string()
}

pub fn square_cover(url: &str) -> String {
    let mut encoded = String::with_capacity(url.len() + 16);
    for c in url.chars() {
        match c {
            ':' | '/' | '?' | '&' | '=' | '#' | '+' | ' ' => {
                encoded.push_str(&format!("%{:02X}", c as u32))
            }
            _ => encoded.push(c),
        }
    }
    format!(
        "{RESIZE_SERVICE}?url={encoded}&w={side}&h={side}&fit=cover&output=jpg",
        side = crate::model::COVER_PX
    )
}

pub struct Button {
    pub label: String,
    pub url: String,
}

pub struct Presence {
    pub details: Option<String>,
    pub state: Option<String>,
    pub large_image: Option<String>,
    pub large_text: Option<String>,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
    pub window: Option<(i64, i64)>,
    pub buttons: Vec<Button>,
}

impl Presence {
    fn activity(&self) -> Value {
        let mut a = serde_json::Map::new();
        a.insert("type".into(), json!(ACTIVITY_LISTENING));
        if let Some(d) = &self.details {
            a.insert("details".into(), json!(d));
        }
        if let Some(s) = &self.state {
            a.insert("state".into(), json!(s));
        }

        let mut assets = serde_json::Map::new();
        if let Some(img) = &self.large_image {
            assets.insert("large_image".into(), json!(img));
            assets.insert(
                "large_text".into(),
                json!(self.large_text.clone().unwrap_or_default()),
            );
        }
        if let Some(img) = &self.small_image {
            assets.insert("small_image".into(), json!(img));
            assets.insert(
                "small_text".into(),
                json!(self.small_text.clone().unwrap_or_default()),
            );
        }
        a.insert("assets".into(), Value::Object(assets));

        if let Some((start, end)) = self.window {
            a.insert("timestamps".into(), json!({ "start": start, "end": end }));
        }
        if !self.buttons.is_empty() {
            let buttons: Vec<Value> = self
                .buttons
                .iter()
                .take(2)
                .map(|b| json!({ "label": b.label, "url": b.url }))
                .collect();
            a.insert("buttons".into(), json!(buttons));
        }
        Value::Object(a)
    }

    fn frame(&self, pid: u32, nonce: &str) -> Value {
        json!({
            "cmd": "SET_ACTIVITY",
            "nonce": nonce,
            "args": { "pid": pid, "activity": self.activity() },
        })
    }
}

fn socket_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        dirs.push(PathBuf::from(rt));
    }
    dirs.push(PathBuf::from("/tmp"));
    dirs.push(PathBuf::from("/run"));
    dirs.push(PathBuf::from("/var/run"));
    dirs
}

fn socket_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in socket_dirs() {
        for n in 0..10 {
            out.push(dir.join(format!("discord-ipc-{n}")));
        }
    }
    out
}

async fn send_frame(stream: &mut UnixStream, op: u32, payload: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if body.len() > u32::MAX as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame).await
}

fn take_frame(buf: &mut Vec<u8>) -> Option<(u32, Value)> {
    if buf.len() < 8 {
        return None;
    }
    let op = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let len = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    if buf.len() < 8 + len {
        return None;
    }
    let body = buf[8..8 + len].to_vec();
    buf.drain(..8 + len);
    serde_json::from_slice(&body).ok().map(|v| (op, v))
}

pub struct Discord {
    stream: Option<UnixStream>,
    nonce: u64,
    enabled: bool,
    sent: Option<String>,
    last_complaint: Option<std::time::Instant>,
    reply: Option<String>,
}

impl Default for Discord {
    fn default() -> Self {
        Self {
            stream: None,
            nonce: 0,
            enabled: false,
            sent: None,
            last_complaint: None,
            reply: None,
        }
    }
}

impl Discord {
    fn complain(&mut self, msg: impl AsRef<str>) {
        let now = std::time::Instant::now();
        if self
            .last_complaint
            .is_some_and(|t| now.duration_since(t) < COMPLAINT_PAUSE)
        {
            return;
        }
        self.last_complaint = Some(now);
        log::warn!("{}", msg.as_ref());
    }

    fn note(&mut self, msg: impl AsRef<str>) {
        self.last_complaint = None;
        log::info!("{}", msg.as_ref());
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn reply(&self) -> Option<&str> {
        self.reply.as_deref()
    }

    pub async fn settle(&mut self, timeout: Duration) -> Option<&str> {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.reply.is_none() {
            self.drain().await;
            if self.reply.is_some() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.reply()
    }

    pub fn set_enabled(&mut self, on: bool) {
        if self.enabled == on {
            return;
        }
        self.enabled = on;
        self.sent = None;
        if on {
            log::info!("presence enabled");
        } else {
            self.clear();
            log::info!("presence disabled, activity cleared");
        }
    }

    fn clear(&mut self) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let frame = json!({
            "cmd": "SET_ACTIVITY",
            "nonce": nonce,
            "args": { "pid": std::process::id(), "activity": Value::Null },
        });
        if let Ok(body) = serde_json::to_vec(&frame) {
            let mut buf = Vec::with_capacity(8 + body.len());
            buf.extend_from_slice(&OPCODE_FRAME.to_le_bytes());
            buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
            buf.extend_from_slice(&body);
            let _ = stream.try_write(&buf);
        }
    }

    async fn connect(&mut self) -> bool {
        if self.stream.is_some() {
            return true;
        }
        let mut complaints = Vec::new();
        for path in socket_paths().into_iter().filter(|p| p.exists()) {
            log::debug!("trying {}", path.display());
            match self.handshake(path.clone()).await {
                Ok(stream) => {
                    self.note(format!("connected to {} as {APP_ID}", path.display()));
                    self.stream = Some(stream);
                    for c in complaints {
                        log::warn!("{c}");
                    }
                    return true;
                }
                Err(why) => {
                    log::debug!("{} refused: {why}", path.display());
                    complaints.push(format!("{}: {why}", path.display()));
                }
            }
        }
        if complaints.is_empty() {
            self.complain(
                "no discord-ipc socket found -- is the Discord desktop app running? \
                 (looked in $XDG_RUNTIME_DIR, /tmp, /run, /var/run)",
            );
        } else {
            self.complain(format!(
                "no usable discord-ipc socket. {}",
                complaints.join("; ")
            ));
        }
        false
    }

    async fn handshake(&self, path: PathBuf) -> Result<UnixStream, String> {
        let mut stream = UnixStream::connect(&path)
            .await
            .map_err(|e| format!("connect failed: {e}"))?;
        let hello = json!({ "v": 1, "client_id": APP_ID });
        send_frame(&mut stream, OPCODE_HANDSHAKE, &hello)
            .await
            .map_err(|e| format!("handshake write failed: {e}"))?;

        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some((op, value)) = take_frame(&mut buf) {
                if op == OPCODE_PING {
                    let _ = send_frame(&mut stream, OPCODE_PONG, &Value::Null).await;
                    continue;
                }
                let evt = value.get("evt").and_then(Value::as_str).unwrap_or("?");
                return if op == OPCODE_FRAME && evt == "READY" {
                    Ok(stream)
                } else {
                    Err(format!("unexpected reply: op {op} evt {evt}"))
                };
            }
            if buf.len() > 1 << 20 {
                return Err("handshake reply was implausibly large".into());
            }
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.read(&mut chunk)).await {
                Ok(Ok(0)) => return Err("closed the socket before replying".into()),
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
                Ok(Err(e)) => return Err(format!("read failed: {e}")),
                Err(_) => return Err(format!("no reply within {HANDSHAKE_TIMEOUT:?}")),
            }
        }
    }

    async fn drain(&mut self) -> bool {
        let Some(stream) = self.stream.as_mut() else {
            return false;
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.try_read(&mut chunk) {
                Ok(0) => return false,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return false,
            }
        }
        let mut complaints = Vec::new();
        while let Some((op, value)) = take_frame(&mut buf) {
            if op == OPCODE_PING {
                let _ = send_frame(stream, OPCODE_PONG, &value).await;
                continue;
            }
            if let Some(evt) = value.get("evt").and_then(Value::as_str) {
                match evt {
                    "ERROR" => {
                        let d = value.get("data");
                        let code = d.and_then(|d| d.get("code")).and_then(Value::as_i64);
                        let msg = d.and_then(|d| d.get("message")).and_then(Value::as_str);
                        let text = format!("rejected: code {code:?} {msg:?}");
                        self.reply = Some(text.clone());
                        complaints.push(format!("discord rejected the activity: {text}"));
                    }
                    "ACTIVITY_UPDATE" => {
                        self.reply = Some("accepted".to_string());
                        log::info!("discord acknowledged the activity");
                    }
                    other => log::info!("discord event {other}"),
                }
            }
        }
        for c in complaints {
            self.complain(c);
        }
        true
    }

    pub async fn publish(&mut self, presence: &Presence) {
        if !self.enabled {
            return;
        }
        let wanted = presence.activity().to_string();
        if !self.connect().await {
            return;
        }
        if !self.drain().await {
            self.complain("discord closed the socket; will reconnect");
            self.stream = None;
            self.sent = None;
            return;
        }
        if self.sent.as_deref() == Some(wanted.as_str()) {
            return;
        }
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let frame = presence.frame(std::process::id(), &nonce);
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        if let Err(e) = send_frame(stream, OPCODE_FRAME, &frame).await {
            self.complain(format!("sending the activity failed: {e}"));
            self.stream = None;
            self.sent = None;
            return;
        }
        self.note(format!("activity sent: {wanted}"));
        self.sent = Some(wanted);
    }

    pub fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}