use std::time::Duration;

use diqwest::WithDigestAuth;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server url: {0}")]
    Url(#[from] url::ParseError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("digest auth error: {0}")]
    Digest(#[from] diqwest::error::Error),
    #[error("authentication failed")]
    Unauthorized,
    #[error("server returned {0}")]
    Status(StatusCode),
    #[error("bad response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("response larger than {0} bytes")]
    TooLarge(usize),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Largest API reply we accept (a full lineup's EPG block is a few MB).
const MAX_API_BYTES: usize = 64 << 20;
/// Largest icon/other resource we accept.
const MAX_FETCH_BYTES: usize = 2 << 20;

/// Reads a body, refusing anything over `max` bytes (by Content-Length or while streaming).
async fn read_capped(mut resp: Response, max: usize) -> Result<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(Error::TooLarge(max));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > max {
            return Err(Error::TooLarge(max));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Auth {
    #[default]
    None,
    Basic { user: String, pass: String },
    Digest { user: String, pass: String },
}

/// Cheap to clone; share one per server.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    auth: Auth,
}

impl Client {
    /// `server` is e.g. `http://192.168.1.10:9981` (a trailing path such as `/tvh` is kept).
    pub fn new(server: &str, auth: Auth) -> Result<Self> {
        let mut base = Url::parse(server)?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        // reqwest is built without a bundled TLS provider; use ring (verifies against the
        // system CA store via rustls-platform-verifier). Errors only mean one is already set.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { http, base, auth })
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path.trim_start_matches('/'))?)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Response> {
        let resp = match &self.auth {
            Auth::None => req.send().await?,
            Auth::Basic { user, pass } => req.basic_auth(user, Some(pass)).send().await?,
            Auth::Digest { user, pass } => req.send_digest_auth((user.as_str(), pass.as_str())).await?,
        };
        match resp.status() {
            s if s.is_success() => Ok(resp),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Unauthorized),
            s => Err(Error::Status(s)),
        }
    }

    /// POSTs form parameters to an `/api/...` endpoint and decodes the JSON reply.
    async fn api<T: DeserializeOwned>(&self, path: &str, form: &[(&str, String)]) -> Result<T> {
        let req = self.http.post(self.url(path)?).form(form);
        let body = read_capped(self.send(req).await?, MAX_API_BYTES).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// For endpoints whose reply we don't care about (often `{}`).
    async fn api_call(&self, path: &str, form: &[(&str, String)]) -> Result<()> {
        let req = self.http.post(self.url(path)?).form(form);
        self.send(req).await?;
        Ok(())
    }

    pub async fn server_info(&self) -> Result<ServerInfo> {
        self.api("api/serverinfo", &[]).await
    }

    /// Enabled channels sorted by number.
    pub async fn channels(&self) -> Result<Vec<Channel>> {
        let grid: Grid<Channel> = self.api("api/channel/grid", &[("limit", "100000".into())]).await?;
        let mut chans: Vec<_> = grid.entries.into_iter().filter(|c| c.enabled).collect();
        chans.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.name.cmp(&b.name)));
        Ok(chans)
    }

    /// Enabled, user-visible tags sorted by their configured index.
    pub async fn tags(&self) -> Result<Vec<ChannelTag>> {
        let grid: Grid<ChannelTag> = self.api("api/channeltag/grid", &[("limit", "10000".into())]).await?;
        let mut tags: Vec<_> = grid.entries.into_iter().filter(|t| t.enabled && !t.internal).collect();
        tags.sort_by(|a, b| a.index.cmp(&b.index).then_with(|| a.name.cmp(&b.name)));
        Ok(tags)
    }

    /// All events overlapping `[from, to)` (unix seconds), optionally for one channel.
    pub async fn epg(&self, channel: Option<&str>, from: i64, to: i64) -> Result<Vec<EpgEvent>> {
        let filter = json!([
            { "type": "numeric", "field": "stop", "comparison": "gt", "value": from },
            { "type": "numeric", "field": "start", "comparison": "lt", "value": to },
        ]);
        let mut form = vec![
            ("limit", "100000".to_string()),
            ("filter", filter.to_string()),
            ("sort", "start".into()),
            ("dir", "ASC".into()),
        ];
        if let Some(ch) = channel {
            form.push(("channel", ch.to_string()));
        }
        let grid: Grid<EpgEvent> = self.api("api/epg/events/grid", &form).await?;
        Ok(grid.entries)
    }

    pub async fn dvr_upcoming(&self) -> Result<Vec<DvrEntry>> {
        self.dvr_grid("api/dvr/entry/grid_upcoming").await
    }

    pub async fn dvr_finished(&self) -> Result<Vec<DvrEntry>> {
        self.dvr_grid("api/dvr/entry/grid_finished").await
    }

    async fn dvr_grid(&self, path: &str) -> Result<Vec<DvrEntry>> {
        let form = [("limit", "100000".into()), ("sort", "start".into()), ("dir", "ASC".into())];
        let grid: Grid<DvrEntry> = self.api(path, &form).await?;
        Ok(grid.entries)
    }

    pub async fn autorecs(&self) -> Result<Vec<AutorecRule>> {
        let grid: Grid<AutorecRule> = self.api("api/dvr/autorec/grid", &[("limit", "10000".into())]).await?;
        Ok(grid.entries)
    }

    /// Schedules a one-off recording of an EPG event using the default DVR profile.
    pub async fn record_event(&self, event_id: u64) -> Result<()> {
        let form = [("event_id", event_id.to_string()), ("config_uuid", String::new())];
        self.api_call("api/dvr/entry/create_by_event", &form).await
    }

    /// Creates a series-link (autorec) rule from an EPG event.
    pub async fn record_series(&self, event_id: u64) -> Result<()> {
        let form = [("event_id", event_id.to_string()), ("config_uuid", String::new())];
        self.api_call("api/dvr/autorec/create_by_series", &form).await
    }

    /// Cancels a scheduled entry, or stops an in-progress recording (keeping the file).
    pub async fn dvr_cancel(&self, uuid: &str) -> Result<()> {
        self.api_call("api/dvr/entry/cancel", &[("uuid", uuid.into())]).await
    }

    /// Deletes a finished recording and its file.
    pub async fn dvr_remove(&self, uuid: &str) -> Result<()> {
        self.api_call("api/dvr/entry/remove", &[("uuid", uuid.into())]).await
    }

    /// Deletes any idnode (e.g. an autorec rule).
    pub async fn idnode_delete(&self, uuid: &str) -> Result<()> {
        self.api_call("api/idnode/delete", &[("uuid", uuid.into())]).await
    }

    /// Absolute URL for a channel icon path from [`Channel::icon_public_url`].
    pub fn icon_url(&self, icon: &str) -> Option<Url> {
        if icon.starts_with("http://") || icon.starts_with("https://") {
            Url::parse(icon).ok()
        } else {
            self.url(icon).ok()
        }
    }

    /// Whether `u` is on the Tvheadend server itself (same scheme, host and port).
    pub fn is_same_origin(&self, u: &Url) -> bool {
        u.scheme() == self.base.scheme()
            && u.host_str() == self.base.host_str()
            && u.port_or_known_default() == self.base.port_or_known_default()
    }

    /// Fetches an icon (or other resource) as raw bytes. Credentials are only sent to the
    /// Tvheadend server itself, never to external icon hosts.
    pub async fn fetch(&self, url: Url) -> Result<Vec<u8>> {
        let resp = if self.is_same_origin(&url) {
            self.send(self.http.get(url)).await?
        } else {
            let resp = self.http.get(url).send().await?;
            if !resp.status().is_success() {
                return Err(Error::Status(resp.status()));
            }
            resp
        };
        read_capped(resp, MAX_FETCH_BYTES).await
    }

    /// Live stream URL for the player. Credentials are embedded as userinfo, which
    /// ffmpeg's HTTP client handles for both basic and digest auth.
    pub fn stream_url(&self, channel_uuid: &str, profile: &str) -> Url {
        let mut u = self.url(&format!("stream/channel/{channel_uuid}")).expect("valid base");
        u.query_pairs_mut().append_pair("profile", profile);
        self.with_credentials(u)
    }

    /// Playback URL for a finished (or in-progress) recording.
    pub fn recording_url(&self, dvr_uuid: &str) -> Url {
        let u = self.url(&format!("dvrfile/{dvr_uuid}")).expect("valid base");
        self.with_credentials(u)
    }

    fn with_credentials(&self, mut u: Url) -> Url {
        if let Auth::Basic { user, pass } | Auth::Digest { user, pass } = &self.auth {
            let _ = u.set_username(user);
            let _ = u.set_password(Some(pass));
        }
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_base_path_and_embed_credentials() {
        let c = Client::new(
            "http://tv.lan:9981/tvh",
            Auth::Digest { user: "bob".into(), pass: "p@ss".into() },
        )
        .unwrap();
        assert_eq!(
            c.stream_url("abc", "pass").as_str(),
            "http://bob:p%40ss@tv.lan:9981/tvh/stream/channel/abc?profile=pass"
        );
        assert_eq!(c.recording_url("x").as_str(), "http://bob:p%40ss@tv.lan:9981/tvh/dvrfile/x");
        assert_eq!(c.icon_url("imagecache/3").unwrap().as_str(), "http://tv.lan:9981/tvh/imagecache/3");
    }

    #[test]
    fn same_origin_matrix() {
        let c = Client::new("http://tv.lan:9981/tvh", Auth::None).unwrap();
        let yes = ["http://tv.lan:9981/imagecache/1", "http://tv.lan:9981/other/path"];
        let no = [
            "https://tv.lan:9981/imagecache/1", // scheme
            "http://tv.lan/imagecache/1",       // port (80)
            "http://evil.example:9981/x.png",   // host
            "http://tv.lan.evil.example:9981/", // suffix trick
        ];
        for u in yes {
            assert!(c.is_same_origin(&Url::parse(u).unwrap()), "{u}");
        }
        for u in no {
            assert!(!c.is_same_origin(&Url::parse(u).unwrap()), "{u}");
        }
        let https = Client::new("https://tv.example", Auth::None).unwrap();
        assert!(https.is_same_origin(&Url::parse("https://tv.example:443/a").unwrap()));
    }

    #[test]
    fn no_auth_has_no_userinfo() {
        let c = Client::new("http://tv.lan:9981", Auth::None).unwrap();
        assert_eq!(c.stream_url("abc", "pass").as_str(), "http://tv.lan:9981/stream/channel/abc?profile=pass");
    }
}
