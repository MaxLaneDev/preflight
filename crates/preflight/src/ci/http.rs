use super::{Provider, Source};
use anyhow::{Result, ensure};
use reqwest::{Url, header};
use serde_json::Value;
use std::{fmt, time::Duration};
#[derive(Debug)]
pub enum FetchError {
    Http(u16),
    RateLimited(i64),
    Network,
    BodyLimit,
    Budget,
    UnsafeRedirect,
    RangeUnsupported,
    InvalidJson,
}
impl FetchError {
    pub fn retry_at(&self) -> Option<i64> {
        if let Self::RateLimited(t) = self {
            Some(*t)
        } else {
            None
        }
    }
}
impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(code) => write!(f, "provider HTTP {code}"),
            Self::RateLimited(t) => {
                write!(f, "provider rate limited; retry at Unix milliseconds {t}")
            }
            Self::Network => write!(f, "provider network request failed or timed out"),
            Self::BodyLimit => write!(f, "provider response exceeds import byte limit"),
            Self::Budget => write!(f, "import request budget reached; continue with a new sync"),
            Self::UnsafeRedirect => {
                write!(f, "provider redirect or pagination origin/path not allowed")
            }
            Self::RangeUnsupported => write!(
                f,
                "provider ignored continuation range; retained prefix is incomplete"
            ),
            Self::InvalidJson => write!(f, "invalid provider JSON response"),
        }
    }
}
impl std::error::Error for FetchError {}
pub struct Body {
    pub bytes: Vec<u8>,
    pub complete: bool,
    pub next: Option<Url>,
}
pub struct Http {
    client: reqwest::Client,
    source: Source,
    base: Url,
    token: Option<String>,
    pub remaining: usize,
    pub rate_at: Option<i64>,
}
fn retry_after(value: &str, now: i64) -> Option<i64> {
    let seconds = value.parse::<i64>().ok().or_else(|| {
        let date = jiff::civil::DateTime::strptime("%a, %d %b %Y %H:%M:%S GMT", value).ok()?;
        let at = date
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()?
            .timestamp()
            .as_millisecond();
        Some((at.saturating_sub(now).saturating_add(999) / 1000).max(0))
    })?;
    Some(seconds.clamp(1, 86400))
}
impl Http {
    pub fn new(source: &Source) -> Result<Self> {
        source.validate()?;
        let token = source
            .token_env
            .as_ref()
            .map(|name| {
                std::env::var(name).map_err(|_| {
                    anyhow::anyhow!("configured CI token environment variable is missing")
                })
            })
            .transpose()?;
        ensure!(
            token.as_ref().is_none_or(|v| !v.is_empty()
                && v.len() <= 8192
                && !v.chars().any(char::is_control)),
            "configured CI token is empty or invalid"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(10))
                .build()?,
            source: source.clone(),
            base: source.base()?,
            token,
            remaining: 32,
            rate_at: None,
        })
    }
    pub fn redact(&self, text: &str) -> String {
        let value = if let Some(token) = &self.token {
            text.replace(token, "[REDACTED]")
        } else {
            text.into()
        };
        crate::log_search::redact(&value)
    }
    pub fn api(&self, parts: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| FetchError::UnsafeRedirect)?
            .pop_if_empty()
            .extend(parts);
        Ok(url)
    }
    pub fn github(&self, tail: &[&str]) -> Result<Url> {
        let mut parts = vec!["repos"];
        parts.extend(self.source.repository.split('/'));
        parts.extend(tail);
        self.api(&parts)
    }
    pub fn gitlab(&self, tail: &[&str]) -> Result<Url> {
        let mut parts = vec!["projects", self.source.repository.as_str()];
        parts.extend(tail);
        self.api(&parts)
    }
    pub fn page(mut url: Url) -> Url {
        url.query_pairs_mut().append_pair("per_page", "100");
        url
    }
    fn allowed_download(&self, url: &Url) -> bool {
        if url.origin() == self.base.origin() {
            return url.scheme() == "https" || self.source.insecure_allowed(url);
        }
        if url.scheme() != "https" && !self.source.insecure_allowed(url) {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        self.source
            .download_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
            || matches!(self.source.provider, Provider::Github)
                && (host.ends_with(".githubusercontent.com")
                    || host.ends_with(".blob.core.windows.net"))
    }
    pub fn cursor_url(&self, raw: &str, expected: &Url) -> Result<Url> {
        let url = Url::parse(raw).map_err(|_| FetchError::UnsafeRedirect)?;
        ensure!(
            url.origin() == expected.origin()
                && url.path() == expected.path()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            FetchError::UnsafeRedirect
        );
        Ok(url)
    }
    pub async fn json(&mut self, url: Url) -> Result<(Value, Body)> {
        let body = self.get(url, 8 * 1024 * 1024, None, None).await?;
        ensure!(body.complete, FetchError::BodyLimit);
        let json = serde_json::from_slice(&body.bytes).map_err(|_| FetchError::InvalidJson)?;
        Ok((json, body))
    }
    pub async fn get(
        &mut self,
        mut url: Url,
        limit: usize,
        offset: Option<usize>,
        etag: Option<&str>,
    ) -> Result<Body> {
        if let Some(at) = self.rate_at.filter(|t| *t > crate::db::now()) {
            return Err(FetchError::RateLimited(at).into());
        }
        ensure!(limit <= 32 * 1024 * 1024, FetchError::BodyLimit);
        ensure!(
            self.allowed_download(&url) && url.username().is_empty() && url.password().is_none(),
            FetchError::UnsafeRedirect
        );
        let initial_path = url.path().to_string();
        let mut redirects = 0;
        let mut retries = 0;
        loop {
            ensure!(self.remaining > 0, FetchError::Budget);
            self.remaining -= 1;
            let mut request = self
                .client
                .get(url.clone())
                .header(header::USER_AGENT, "Preflight-read-only-CI/1");
            if url.origin() == self.base.origin() {
                request = request.header(header::ACCEPT, "application/json");
                if matches!(self.source.provider, Provider::Github) {
                    request = request.header("X-GitHub-Api-Version", "2022-11-28");
                }
                if let Some(token) = &self.token {
                    request = match self.source.provider {
                        Provider::Github => request.bearer_auth(token),
                        Provider::Gitlab => request.header("PRIVATE-TOKEN", token),
                    };
                }
            }
            if let Some(start) = offset {
                request = request.header(
                    header::RANGE,
                    format!(
                        "bytes={start}-{}",
                        start.saturating_add(limit).saturating_sub(1)
                    ),
                );
            }
            if let Some(value) = etag {
                request = request.header(header::IF_NONE_MATCH, value);
            }
            let mut response = match request.send().await {
                Ok(r) => r,
                Err(_) => {
                    if retries < 1 {
                        retries += 1;
                        continue;
                    }
                    return Err(FetchError::Network.into());
                }
            };
            let status = response.status().as_u16();
            if status == 429
                || status == 403
                    && response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|v| v == "0")
            {
                let now = crate::db::now();
                let seconds = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| retry_after(v, now));
                let reset = response
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(|v| v.saturating_mul(1000))
                    .filter(|v| *v >= now && *v <= now + 86400000);
                let at = seconds
                    .map(|s| now + s * 1000)
                    .or(reset)
                    .unwrap_or(now + 60000);
                self.rate_at = Some(at);
                return Err(FetchError::RateLimited(at).into());
            }
            if response.status().is_redirection() && status != 304 {
                ensure!(redirects < 3, FetchError::UnsafeRedirect);
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::UnsafeRedirect)?;
                let next = url.join(location).map_err(|_| FetchError::UnsafeRedirect)?;
                ensure!(
                    self.allowed_download(&next)
                        && next.username().is_empty()
                        && next.password().is_none()
                        && next.fragment().is_none(),
                    FetchError::UnsafeRedirect
                );
                url = next;
                redirects += 1;
                continue;
            }
            if status >= 500 && retries < 1 {
                retries += 1;
                continue;
            }
            ensure!(
                response.status().is_success() || status == 304,
                FetchError::Http(status)
            );
            if offset.is_some_and(|start| start > 0) {
                ensure!(status == 206, FetchError::RangeUnsupported)
            }
            let mut complete = true;
            if status == 206 {
                let range = response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::RangeUnsupported)?;
                let prefix = format!("bytes {}-", offset.unwrap_or(0));
                ensure!(range.starts_with(&prefix), FetchError::RangeUnsupported);
                let (end, total) = range.split_once('/').ok_or(FetchError::RangeUnsupported)?;
                let end = end
                    .rsplit_once('-')
                    .and_then(|(_, v)| v.parse::<usize>().ok())
                    .ok_or(FetchError::RangeUnsupported)?;
                let total = total
                    .parse::<usize>()
                    .map_err(|_| FetchError::RangeUnsupported)?;
                complete = end.saturating_add(1) >= total;
            }
            let next = if url.origin() == self.base.origin() && url.path() == initial_path {
                parse_next(
                    response
                        .headers()
                        .get(header::LINK)
                        .and_then(|v| v.to_str().ok()),
                    &url,
                )?
            } else {
                None
            };
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| FetchError::Network)? {
                let take = chunk.len().min(limit + 1 - bytes.len());
                bytes.extend_from_slice(&chunk[..take]);
                if bytes.len() > limit {
                    bytes.truncate(limit);
                    complete = false;
                    break;
                }
            }
            return Ok(Body {
                bytes,
                complete,
                next,
            });
        }
    }
}
fn parse_next(header: Option<&str>, current: &Url) -> Result<Option<Url>> {
    let Some(value) = header else { return Ok(None) };
    ensure!(value.len() <= 16384, FetchError::UnsafeRedirect);
    for piece in value.split(',') {
        if piece.contains("rel=\"next\"") || piece.contains("rel=next") {
            let raw = piece
                .trim()
                .split_once('<')
                .and_then(|(_, v)| v.split_once('>'))
                .map(|(v, _)| v)
                .ok_or(FetchError::UnsafeRedirect)?;
            let next = current.join(raw).map_err(|_| FetchError::UnsafeRedirect)?;
            ensure!(
                next.origin() == current.origin()
                    && next.path() == current.path()
                    && next.username().is_empty()
                    && next.password().is_none()
                    && next.fragment().is_none(),
                FetchError::UnsafeRedirect
            );
            return Ok(Some(next));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn retry_after_supports_seconds_and_http_date() {
        let now = "2026-10-29T10:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        assert_eq!(super::retry_after("120", now), Some(120));
        assert_eq!(
            super::retry_after("Thu, 29 Oct 2026 10:01:00 GMT", now),
            Some(60)
        );
        assert_eq!(super::retry_after("999999999", now), Some(86400));
        assert_eq!(super::retry_after("invalid", now), None);
    }

    use super::*;
    use axum::{Router, http::HeaderMap, response::IntoResponse, routing::get};
    use std::sync::{Arc, Mutex};
    fn source(base: String, allow: bool) -> Source {
        serde_json::from_value(serde_json::json!({"name":"test","project_id":uuid::Uuid::now_v7().to_string(),"provider":"github","repository":"org/repo","api_base":base,"allow_insecure_loopback":true,"download_hosts":if allow{vec!["127.0.0.1"]}else{vec![]}})).unwrap()
    }
    #[tokio::test]
    async fn redirects_never_forward_api_credentials_and_unknown_origins_are_blocked() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dest = format!("http://{}/blob", target.local_addr().unwrap());
        let stop = tokio_util::sync::CancellationToken::new();
        let signal = stop.clone();
        let app = Router::new().route(
            "/blob",
            get(move |headers: HeaderMap| {
                let seen = capture.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(headers.get(header::AUTHORIZATION).cloned());
                    "bounded data"
                }
            }),
        );
        let second = tokio::spawn(async move {
            axum::serve(target, app)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
                .unwrap()
        });
        let capture = seen.clone();
        let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", endpoint.local_addr().unwrap());
        let app = Router::new().route(
            "/redirect",
            get(move |headers: HeaderMap| {
                let target = dest.clone();
                let seen = capture.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(headers.get(header::AUTHORIZATION).cloned());
                    (reqwest::StatusCode::FOUND, [(header::LOCATION, target)]).into_response()
                }
            }),
        );
        let signal = stop.clone();
        let first = tokio::spawn(async move {
            axum::serve(endpoint, app)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
                .unwrap()
        });
        let mut client = Http::new(&source(base.clone(), true)).unwrap();
        client.token = Some("fixture-api-credential".into());
        let body = client
            .get(
                Url::parse(&(base.clone() + "redirect")).unwrap(),
                1024,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(body.bytes, b"bounded data");
        let headers = seen.lock().unwrap().clone();
        assert_eq!(
            headers[0].as_ref().unwrap(),
            "Bearer fixture-api-credential"
        );
        assert!(headers[1].is_none());
        let mut blocked = Http::new(&source(base.clone(), false)).unwrap();
        let before = seen.lock().unwrap().len();
        let result = blocked
            .get(Url::parse(&(base + "redirect")).unwrap(), 1024, None, None)
            .await;
        let Err(error) = result else {
            panic!("unsafe redirect was allowed")
        };
        assert!(matches!(
            error.downcast_ref::<FetchError>(),
            Some(FetchError::UnsafeRedirect)
        ));
        assert_eq!(seen.lock().unwrap().len(), before + 1);
        stop.cancel();
        first.await.unwrap();
        second.await.unwrap();
    }
    #[test]
    fn credentials_urls_http_nonloopback_and_unsafe_cursor_paths_are_rejected() {
        for url in [
            "http://example.com/",
            "https://user:password@example.com/",
            "https://example.com/?token=secret",
        ] {
            assert!(source(url.into(), false).validate().is_err());
        }
        let client = Http::new(&source("http://127.0.0.1:12345/".into(), false)).unwrap();
        let expected = client.github(&["actions", "runs"]).unwrap();
        assert!(
            client
                .cursor_url("http://127.0.0.1:12345/admin", &expected)
                .is_err()
        );
        assert!(
            client
                .cursor_url(
                    "http://127.0.0.1:23456/repos/org/repo/actions/runs",
                    &expected
                )
                .is_err()
        );
    }
}
