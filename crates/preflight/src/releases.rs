//! Optional, unauthenticated release notifications. Never downloads or installs binaries.
use crate::engine::Engine;
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(6 * 3600);
const MANUAL_INTERVAL: Duration = Duration::from_secs(60);
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_NOTES: usize = 32_000;

#[derive(Default)]
pub(crate) struct Monitor {
    state: Mutex<State>,
    gate: tokio::sync::Mutex<()>,
    revision: AtomicU64,
}
#[derive(Default)]
struct State {
    repository: Option<String>,
    attempted: Option<Instant>,
    checked_at: Option<u64>,
    release: Option<Release>,
    error: Option<String>,
}
#[derive(Clone, Serialize)]
struct Release {
    version: String,
    url: String,
    changelog: String,
    changelog_truncated: bool,
    update_available: bool,
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub details: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub admin_token: String,
    #[serde(default)]
    pub details: bool,
}
pub fn validate(repository: &str) -> Result<()> {
    let parts: Vec<_> = repository.split('/').collect();
    ensure!(
        parts.len() == 2,
        "release_repository must be owner/repository"
    );
    ensure!(
        parts.iter().all(|part| !part.is_empty()
            && part.len() <= 100
            && *part != "."
            && *part != ".."
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))),
        "release_repository must contain two ASCII names using letters, digits, hyphens, underscores or periods"
    );
    Ok(())
}
pub fn revision(e: &Engine) -> u64 {
    e.releases.revision.load(Ordering::Relaxed)
}
pub fn status(e: &Engine, details: bool) -> Value {
    let repository = e.current_config().release_repository;
    let state = e.releases.state.lock().unwrap();
    let matching = state.repository == repository;
    let release = matching.then_some(state.release.as_ref()).flatten();
    let error = matching.then_some(state.error.as_ref()).flatten();
    let status = if repository.is_none() {
        "disabled"
    } else if error.is_some() {
        "error"
    } else if release.is_some_and(|r| r.update_available) {
        "available"
    } else if matching && state.checked_at.is_some() {
        "current"
    } else {
        "unchecked"
    };
    let mut value = json!({"mode":"notify","status":status,"repository":repository,"current_version":env!("CARGO_PKG_VERSION"),"checked_at":matching.then_some(state.checked_at).flatten(),"check_interval_seconds":INTERVAL.as_secs(),"manual_interval_seconds":MANUAL_INTERVAL.as_secs(),"update_available":release.is_some_and(|r|r.update_available),"error":error});
    if let Some(release) = release {
        value["latest_version"] = json!(release.version);
        value["release_url"] = json!(release.url);
        if details {
            value["changelog"] = json!(release.changelog);
            value["changelog_truncated"] = json!(release.changelog_truncated);
        }
    }
    value
}
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    draft: bool,
    prerelease: bool,
}
fn parse(bytes: &[u8], repository: &str) -> Result<Release> {
    let release: GithubRelease =
        serde_json::from_slice(bytes).context("invalid GitHub release response")?;
    ensure!(
        !release.draft && !release.prerelease,
        "latest release must be stable and published"
    );
    let version = semver::Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .context("release tag must be a semantic version")?;
    ensure!(
        version.pre.is_empty(),
        "latest release must not be a prerelease"
    );
    ensure!(release.tag_name.len() <= 128, "release tag exceeds limit");
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let notes = release.body.unwrap_or_default();
    let mut url = reqwest::Url::parse(&format!("https://github.com/{repository}/releases/tag/"))?;
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&release.tag_name);
    Ok(Release {
        version: version.to_string(),
        url: url.into(),
        changelog: notes.chars().take(MAX_NOTES).collect(),
        changelog_truncated: notes.chars().count() > MAX_NOTES,
        update_available: version.cmp_precedence(&current).is_gt(),
    })
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("Preflight/", env!("CARGO_PKG_VERSION")))
        .build()?)
}
async fn fetch(client: &reqwest::Client, endpoint: &str, repository: &str) -> Result<Release> {
    let mut response = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("GitHub release request failed")?;
    ensure!(
        response.status().is_success(),
        "GitHub release request returned HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_RESPONSE as u64),
        "GitHub release response exceeds limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("GitHub release response interrupted")?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE,
            "GitHub release response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    parse(&bytes, repository)
}
pub async fn request(e: &Engine, request: Check) -> Result<Value> {
    crate::settings::authorize(e, &request.admin_token)?;
    check(e, true).await?;
    Ok(status(e, request.details))
}
async fn check(e: &Engine, manual: bool) -> Result<()> {
    let _gate = e.releases.gate.lock().await;
    let repository = e.current_config().release_repository;
    {
        let mut state = e.releases.state.lock().unwrap();
        if state.repository != repository {
            *state = State {
                repository: repository.clone(),
                ..Default::default()
            };
            e.releases.revision.fetch_add(1, Ordering::Relaxed);
        }
        if repository.is_none()
            || state
                .attempted
                .is_some_and(|at| at.elapsed() < if manual { MANUAL_INTERVAL } else { INTERVAL })
        {
            return Ok(());
        }
        state.attempted = Some(Instant::now());
    }
    let repository = repository.unwrap();
    validate(&repository)?;
    let result = match client() {
        Ok(client) => {
            fetch(
                &client,
                &format!("https://api.github.com/repos/{repository}/releases/latest"),
                &repository,
            )
            .await
        }
        Err(error) => Err(error),
    };
    complete(e, &repository, result)
}
fn complete(e: &Engine, repository: &str, result: Result<Release>) -> Result<()> {
    // A hot-reloaded repository must never receive the previous repository's result.
    if e.current_config().release_repository.as_deref() != Some(repository) {
        return Ok(());
    }
    let mut state = e.releases.state.lock().unwrap();
    state.checked_at = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64,
    );
    state.release = None;
    state.error = None;
    match result {
        Ok(release) => state.release = Some(release),
        Err(error) => state.error = Some(error.to_string()),
    }
    e.releases.revision.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
pub async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    loop {
        tokio::select! { biased; _=stop.cancelled()=>break, _=check(&e,false)=>{} }
        tokio::select! { _=stop.cancelled()=>break, _=tokio::time::sleep(Duration::from_secs(10))=>{} }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"tag_name":tag,"body":"<script>untrusted()</script>","draft":false,"prerelease":false})).unwrap()
    }
    #[test]
    fn repositories_are_names_not_urls_or_paths() {
        for name in ["owner/preflight", "a.b/repo_2-test"] {
            validate(name).unwrap();
        }
        for name in [
            "",
            "owner",
            "a/b/c",
            "a/..",
            "../b",
            "/repo",
            "a/b?token=x",
            "https://github.com/a/b",
            "a/b\n",
            "a/%2f",
        ] {
            assert!(validate(name).is_err(), "{name:?}");
        }
    }
    #[test]
    fn product_semver_and_untrusted_changelog_are_bounded() {
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let next = format!("v{}.0.0", current.major + 1);
        let parsed = parse(&release(&next), "owner/repo").unwrap();
        assert!(parsed.update_available);
        assert_eq!(
            parsed.url,
            format!("https://github.com/owner/repo/releases/tag/{next}")
        );
        assert_eq!(parsed.changelog, "<script>untrusted()</script>");
        let mut same = current.clone();
        same.build = semver::BuildMetadata::new("other-build").unwrap();
        assert!(
            !parse(&release(&same.to_string()), "owner/repo")
                .unwrap()
                .update_available
        );
        assert!(
            !parse(&release("v0.0.9"), "owner/repo")
                .unwrap()
                .update_available
        );
        for tag in ["latest", "v1.0.0-beta.1", "1.0.0/../../x"] {
            assert!(parse(&release(tag), "owner/repo").is_err());
        }
        let mut value: Value = serde_json::from_slice(&release("v0.2.0")).unwrap();
        value["body"] = json!("🦀".repeat(MAX_NOTES + 1));
        let parsed = parse(&serde_json::to_vec(&value).unwrap(), "owner/repo").unwrap();
        assert_eq!(parsed.changelog.chars().count(), MAX_NOTES);
        assert!(parsed.changelog_truncated);
        for flag in ["draft", "prerelease"] {
            value[flag] = json!(true);
            assert!(parse(&serde_json::to_vec(&value).unwrap(), "owner/repo").is_err());
            value[flag] = json!(false);
        }
    }
    #[tokio::test]
    async fn public_fetch_is_unauthenticated_and_rejects_redirects_errors_and_oversize() {
        use axum::{
            Router,
            http::{HeaderMap, StatusCode},
            response::Redirect,
            routing::get,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app=Router::new()
            .route("/release",get(|headers: HeaderMap|async move {
                assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("cookie"));
                axum::Json(json!({"tag_name":"v0.2.0","body":"notes","draft":false,"prerelease":false}))
            }))
            .route("/redirect",get(||async{Redirect::temporary("/release")}))
            .route("/limited",get(||async{StatusCode::TOO_MANY_REQUESTS}))
            .route("/oversize",get(||async{"x".repeat(MAX_RESPONSE+1)}))
            .route("/invalid",get(||async{"invalid-json"}));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = client().unwrap();
        let mut results = Vec::new();
        for path in ["release", "redirect", "limited", "oversize", "invalid"] {
            results.push(fetch(&client, &format!("{base}/{path}"), "owner/repo").await);
        }
        server.abort();
        let _ = server.await;
        assert_eq!(results.remove(0).unwrap().version, "0.2.0");
        assert!(results.iter().all(Result::is_err));
    }
    #[tokio::test]
    async fn disabled_monitor_has_no_check_and_shuts_down_promptly() {
        let temp = tempfile::tempdir().unwrap();
        let config:crate::config::Config=serde_json::from_value(json!({"version":1,"bind":"127.0.0.1:0","data_dir":temp.path().join("data"),"allowed_roots":[temp.path()]})).unwrap();
        let e = Engine::open(config).unwrap();
        check(&e, true).await.unwrap();
        assert_eq!(status(&e, false)["status"], "disabled");
        assert!(status(&e, true)["checked_at"].is_null());
        e.live_config.write().unwrap().release_repository = Some("owner/repo".into());
        let attempted = Instant::now();
        {
            let mut state = e.releases.state.lock().unwrap();
            state.repository = Some("owner/repo".into());
            state.attempted = Some(attempted);
            state.release = Some(parse(&release("v0.2.0"), "owner/repo").unwrap());
        }
        check(&e, true).await.unwrap();
        assert_eq!(e.releases.state.lock().unwrap().attempted, Some(attempted));
        assert!(status(&e, false).get("changelog").is_none());
        assert!(status(&e, true)["changelog"].is_string());
        e.live_config.write().unwrap().release_repository = Some("other/repo".into());
        complete(
            &e,
            "owner/repo",
            Ok(parse(&release("v0.2.0"), "owner/repo").unwrap()),
        )
        .unwrap();
        assert_eq!(status(&e, false)["status"], "unchecked");
        assert!(status(&e, true).get("latest_version").is_none());
        e.live_config.write().unwrap().release_repository = None;
        let monitor = tokio::spawn(run(e.clone()));
        e.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), monitor)
            .await
            .unwrap()
            .unwrap();
    }
}
