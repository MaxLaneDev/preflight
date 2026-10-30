//! Optional read-only provider ingestion. No provider execution or mutation endpoints.
mod artifacts;
mod http;
mod providers;
mod store;
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Github,
    Gitlab,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    pub project_id: String,
    pub provider: Provider,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub download_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_insecure_loopback: bool,
}
impl Source {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.api_base.as_ref().is_none_or(|v| v.len() <= 2048)
                && serde_json::to_vec(self)?.len() <= 8192,
            invalid("CI source configuration exceeds URL or record size limit")
        );
        crate::control::validate_name(&self.name)?;
        uuid::Uuid::parse_str(&self.project_id)?;
        ensure!(
            !self.repository.is_empty()
                && self.repository.len() <= 256
                && self.repository.split('/').all(|p| !p.is_empty()
                    && p != "."
                    && p != ".."
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
            invalid("repository must be a numeric GitLab project ID or namespace/repository path")
        );
        if matches!(self.provider, Provider::Github) {
            ensure!(
                self.repository.split('/').count() == 2,
                invalid("GitHub repository must be owner/repository")
            );
        }
        if let Some(name) = &self.token_env {
            ensure!(
                !name.is_empty()
                    && name.len() <= 128
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
                invalid(
                    "token_env must be an uppercase environment variable name, never a token value"
                )
            );
        }
        ensure!(
            self.download_hosts.len() <= 16
                && self.download_hosts.iter().all(|h| !h.is_empty()
                    && h.len() <= 253
                    && h.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))),
            invalid("download_hosts must contain exact DNS hostnames")
        );
        ensure!(
            self.test_artifacts.len() <= 16
                && self
                    .test_artifacts
                    .iter()
                    .all(|n| !n.is_empty() && n.len() <= 256 && !n.chars().any(char::is_control)),
            invalid("test_artifacts must contain bounded exact artifact names")
        );
        if let Some(seconds) = self.poll_seconds {
            ensure!(
                (60..=86400).contains(&seconds),
                invalid("poll_seconds must be 60..86400 or omitted")
            );
        }
        let base = self.base()?;
        ensure!(
            base.query().is_none()
                && base.fragment().is_none()
                && base.username().is_empty()
                && base.password().is_none(),
            invalid("api_base must not contain credentials, query or fragment")
        );
        ensure!(
            base.scheme() == "https" || self.insecure_allowed(&base),
            invalid("CI requires HTTPS; explicit insecure loopback is only for local testing")
        );
        Ok(())
    }
    fn insecure_allowed(&self, url: &reqwest::Url) -> bool {
        self.allow_insecure_loopback
            && url.scheme() == "http"
            && url
                .host_str()
                .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
    }
    fn base(&self) -> Result<reqwest::Url> {
        let default = match self.provider {
            Provider::Github => "https://api.github.com/",
            Provider::Gitlab => "https://gitlab.com/api/v4/",
        };
        let raw = self.api_base.as_deref().unwrap_or(default);
        let mut url = reqwest::Url::parse(raw).map_err(|_| invalid("invalid CI API base URL"))?;
        ensure!(
            url.host_str().is_some(),
            invalid("CI API base requires a host")
        );
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(url)
    }
    fn key(&self) -> Result<String> {
        Ok(crate::hashing::hex(Sha256::digest(serde_json::to_vec(
            &json!({"project":self.project_id,"name":self.name,"provider":self.provider,"api":self.base()?.as_str(),"repository":self.repository,"test_artifacts":self.test_artifacts}),
        )?)))
    }
}
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    pub source: String,
    pub request_id: String,
    pub provider_run_id: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "runs_limit")]
    pub max_runs: usize,
    #[serde(default = "pages_limit")]
    pub max_pages: usize,
}
fn runs_limit() -> usize {
    1
}
fn pages_limit() -> usize {
    2
}
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FetchArtifact {
    pub project_id: String,
    pub run_id: String,
    pub artifact_id: String,
    pub request_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Work {
    Sync(Sync),
    Artifact(FetchArtifact),
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub project_id: String,
    pub sync_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub project_id: String,
    pub source: Option<String>,
    pub before: Option<String>,
    #[serde(default = "list_limit")]
    pub limit: usize,
}
fn list_limit() -> usize {
    20
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Get {
    pub project_id: String,
    pub run_id: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "detail_limit")]
    pub limit: usize,
    #[serde(default = "detail_budget")]
    pub max_bytes: usize,
}
fn detail_limit() -> usize {
    10
}
fn detail_budget() -> usize {
    8192
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceQuery {
    pub project_id: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "detail_limit")]
    pub limit: usize,
}
pub fn sources(e: &Engine, r: &SourceQuery) -> Result<Value> {
    if let Some(project) = &r.project_id {
        e.db.project(project)?;
    }
    ensure!(
        r.offset <= 32 && (1..=32).contains(&r.limit),
        invalid("source offset must be 0..32; limit 1..32")
    );
    let config = e.current_config();
    let values = config
        .ci_sources
        .iter()
        .filter(|s| r.project_id.as_ref().is_none_or(|p| p == &s.project_id))
        .collect::<Vec<_>>();
    let total = values.len();
    let mut list = Vec::new();
    for s in values.into_iter().skip(r.offset).take(r.limit) {
        let retry: Option<i64> =
            e.db.connection()?
                .query_row(
                    "SELECT retry_at FROM ci_source_state WHERE source_key=?1",
                    [s.key()?],
                    |v| v.get(0),
                )
                .optional()?
                .flatten();
        list.push(json!({"name":s.name,"project_id":s.project_id,"provider":s.provider,"repository":s.repository,"api_origin":s.base()?.origin().ascii_serialization(),"credential_status":match &s.token_env { None=>"anonymous", Some(name) if std::env::var(name).is_ok_and(|v|!v.is_empty())=>"configured", Some(_)=>"missing"},"auth":if s.token_env.as_ref().is_some_and(|name|std::env::var(name).is_ok_and(|v|!v.is_empty())){"configured"}else{"anonymous"},"test_report_artifact_count":s.test_artifacts.len(),"poll_seconds":s.poll_seconds,"retry_at":retry}));
    }
    Ok(
        json!({"sources":list,"next_offset":(r.offset+r.limit<total).then_some(r.offset+r.limit),"provider_access":"GET only; completed results; no triggers or remote mutations","configuration":"administrator server_settings ci_sources; token values stay in server environment"}),
    )
}

fn source(e: &Engine, name: &str) -> Result<Source> {
    e.current_config()
        .ci_sources
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or(Fault::NotFound.into())
}
pub fn sync(e: &Engine, r: Sync) -> Result<Value> {
    ensure!(
        (1..=5).contains(&r.max_runs) && (1..=10).contains(&r.max_pages),
        invalid("max_runs must be 1..5; max_pages 1..10")
    );
    ensure!(
        r.provider_run_id.is_none() || r.cursor.is_none(),
        invalid("choose provider_run_id or cursor")
    );
    if let Some(id) = &r.provider_run_id {
        providers::number(id)?;
    }
    let source = source(e, &r.source)?;
    let key = r.request_id.clone();
    enqueue(e, &source, &key, Work::Sync(r))
}
pub fn artifact_fetch(e: &Engine, r: FetchArtifact) -> Result<Value> {
    let c = e.db.connection()?;
    let (name,project,source_key):(String,String,String)=c.query_row("SELECT c.source_name,r.project_id,c.source_key FROM ci_runs c JOIN runs r ON r.id=c.run_id WHERE c.run_id=?1",[&r.run_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?.ok_or(Fault::NotFound)?;
    ensure!(project == r.project_id, Fault::NotFound);
    drop(c);
    let source = source(e, &name)?;
    ensure!(
        source.project_id == r.project_id && source.key()? == source_key,
        Fault::NotFound
    );
    let key = r.request_id.clone();
    enqueue(e, &source, &key, Work::Artifact(r))
}
fn enqueue(e: &Engine, s: &Source, key: &str, work: Work) -> Result<Value> {
    ensure!(!e.is_stopping(), Fault::Busy);
    s.validate()?;
    e.db.project(&s.project_id)?;
    crate::control::validate_name(key)?;
    let request = serde_json::to_string(&work)?;
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT id,request_json FROM ci_syncs WHERE project_id=?1 AND request_id=?2",
            params![s.project_id, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let id = if let Some((id, previous)) = old {
        ensure!(previous == request, Fault::IdempotencyConflict);
        id
    } else {
        let pending: u64 = tx.query_row(
            "SELECT count(*) FROM ci_syncs WHERE state IN ('queued','running')",
            [],
            |r| r.get(0),
        )?;
        ensure!(pending < 32, Fault::Busy);
        let id = uuid::Uuid::now_v7().to_string();
        tx.execute("INSERT INTO ci_syncs(id,source_key,source_name,project_id,request_id,request_json,source_json,state,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'queued',?8)",params![id,s.key()?,s.name,s.project_id,key,request,serde_json::to_string(s)?,crate::db::now()])?;
        id
    };
    tx.commit()?;
    Ok(
        json!({"sync_id":id,"project_id":s.project_id,"status_tool":"ci_sync_status","execution":"background read-only import; no local or provider build started"}),
    )
}
pub fn sync_status(e: &Engine, r: &Status) -> Result<Value> {
    let c = e.db.connection()?;
    let mut row:Value=c.query_row("SELECT state,result_json,error,retry_at,created_at,finished_at,cancel_requested,cancel_actor,cancel_reason FROM ci_syncs WHERE id=?1 AND project_id=?2",params![r.sync_id,r.project_id],|v|Ok(json!({"sync_id":r.sync_id,"project_id":r.project_id,"state":v.get::<_,String>(0)?,"result":v.get::<_,Option<String>>(1)?.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"error":v.get::<_,Option<String>>(2)?,"retry_at":v.get::<_,Option<i64>>(3)?,"created_at":v.get::<_,i64>(4)?,"finished_at":v.get::<_,Option<i64>>(5)?,"cancel_requested":v.get::<_,bool>(6)?,"cancel_actor":v.get::<_,Option<String>>(7)?,"cancel_reason":v.get::<_,Option<String>>(8)?}))).optional()?.ok_or(Fault::NotFound)?;
    if let Some(result) = row["result"].as_object_mut() {
        result.remove("next_url");
        result.remove("next_skip");
    }
    Ok(row)
}
pub fn runs(e: &Engine, r: &List) -> Result<Value> {
    e.db.project(&r.project_id)?;
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT c.run_id,c.source_name,c.provider,c.provider_run_id,c.provider_revision,c.git_sha,c.web_url,c.state,r.status,r.created_at,r.finished_at,p.pipeline_key,p.name,COALESCE(json_extract(c.progress_json,'$.resumable'),0) FROM ci_runs c JOIN runs r ON r.id=c.run_id JOIN run_pipeline_identity p ON p.run_id=r.id WHERE r.project_id=?1 AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.kind='run' AND t.target_id=r.id AND t.restored_at IS NULL) AND (?2 IS NULL OR c.source_name=?2) AND (?3 IS NULL OR c.run_id<?3) ORDER BY c.run_id DESC LIMIT ?4")?.query_map(params![r.project_id,r.source,r.before,r.limit+1],|v|Ok(json!({"run_id":v.get::<_,String>(0)?,"source":v.get::<_,String>(1)?,"provider":v.get::<_,String>(2)?,"provider_run_id":v.get::<_,String>(3)?,"revision":v.get::<_,String>(4)?,"git_sha":v.get::<_,Option<String>>(5)?,"web_url":v.get::<_,Option<String>>(6)?,"import_state":v.get::<_,String>(7)?,"status":v.get::<_,String>(8)?,"created_at":v.get::<_,i64>(9)?,"finished_at":v.get::<_,Option<i64>>(10)?,"pipeline_key":v.get::<_,String>(11)?,"name":v.get::<_,String>(12)?,"resumable":v.get::<_,bool>(13)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > r.limit;
    let next = more.then(|| rows[r.limit - 1]["run_id"].clone());
    Ok(json!({"runs":rows.into_iter().take(r.limit).collect::<Vec<_>>(),"next_cursor":next}))
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub project_id: String,
    pub run_id: String,
    pub artifact_id: String,
}
pub fn artifact_file(e: &Engine, r: &ArtifactFile) -> Result<(Vec<u8>, String)> {
    artifacts::file(e, r)
}
pub fn get(e: &Engine, r: &Get) -> Result<Value> {
    store::get(e, r)
}
pub async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {biased;_=stop.cancelled()=>break,_=tick.tick()=>{}};
        if let Err(error) = poll(&e) {
            eprintln!("CI polling: {error}");
        }
        let work = claim(&e);
        match work {
            Ok(Some((id, s, request))) => {
                let outcome = tokio::select! {biased;_=stop.cancelled()=>break,_=cancelled(&e,&id)=>None,result=providers::execute(&e,&s,&id,request)=>Some(result)};
                let Some(result) = outcome else {
                    if let Ok(c) = e.db.connection() {
                        let _ = c.execute(
                            "UPDATE ci_syncs SET state='cancelled',finished_at=?2 WHERE id=?1",
                            params![id, crate::db::now()],
                        );
                    }
                    continue;
                };
                if let Err(error) = finish(&e, &id, result) {
                    eprintln!("CI import persistence: {error}");
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("CI queue: {error}"),
        }
    }
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub project_id: String,
    pub sync_id: String,
    pub actor: String,
    pub reason: String,
}
pub fn cancel(e: &Engine, r: &Cancel) -> Result<Value> {
    ensure!(
        !r.actor.trim().is_empty()
            && r.actor.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 1024,
        invalid("bounded actor and reason required")
    );
    let c = e.db.connection()?;
    let changed=c.execute("UPDATE ci_syncs SET cancel_requested=1,cancel_actor=COALESCE(cancel_actor,?3),cancel_reason=COALESCE(cancel_reason,?4),state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,finished_at=CASE WHEN state='queued' THEN ?5 ELSE finished_at END WHERE id=?1 AND project_id=?2 AND state IN ('queued','running')",params![r.sync_id,r.project_id,r.actor,r.reason,crate::db::now()])?;
    let _ = changed;
    drop(c);
    sync_status(
        e,
        &Status {
            project_id: r.project_id.clone(),
            sync_id: r.sync_id.clone(),
        },
    )
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncList {
    pub project_id: Option<String>,
    pub source: Option<String>,
    pub before: Option<String>,
    #[serde(default = "list_limit")]
    pub limit: usize,
}
pub fn syncs(e: &Engine, r: &SyncList) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT id,project_id,source_name,state,error,retry_at,created_at,finished_at,cancel_requested FROM ci_syncs WHERE (?1 IS NULL OR project_id=?1) AND (?2 IS NULL OR source_name=?2) AND (?3 IS NULL OR id<?3) ORDER BY id DESC LIMIT ?4")?.query_map(params![r.project_id,r.source,r.before,r.limit+1],|v|Ok(json!({"sync_id":v.get::<_,String>(0)?,"project_id":v.get::<_,String>(1)?,"source":v.get::<_,String>(2)?,"state":v.get::<_,String>(3)?,"error":v.get::<_,Option<String>>(4)?,"retry_at":v.get::<_,Option<i64>>(5)?,"created_at":v.get::<_,i64>(6)?,"finished_at":v.get::<_,Option<i64>>(7)?,"cancel_requested":v.get::<_,bool>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let next = (rows.len() > r.limit).then(|| rows[r.limit - 1]["sync_id"].clone());
    Ok(json!({"imports":rows.into_iter().take(r.limit).collect::<Vec<_>>(),"next_cursor":next}))
}
async fn cancelled(e: &Engine, id: &str) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let stop = e.db.connection().and_then(|c| {
            Ok(c.query_row(
                "SELECT cancel_requested FROM ci_syncs WHERE id=?1",
                [id],
                |r| r.get::<_, bool>(0),
            )?)
        });
        if !matches!(stop, Ok(false)) {
            break;
        }
    }
}
fn claim(e: &Engine) -> Result<Option<(String, Source, Work)>> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let row:Option<(String,String,String)>=tx.query_row("SELECT id,source_json,request_json FROM ci_syncs WHERE state='queued' ORDER BY created_at,id LIMIT 1",[],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?;
    let Some((id, s, r)) = row else {
        return Ok(None);
    };
    tx.execute("UPDATE ci_syncs SET state='running' WHERE id=?1", [&id])?;
    tx.commit()?;
    Ok(Some((
        id,
        serde_json::from_str(&s)?,
        serde_json::from_str(&r)?,
    )))
}
fn finish(e: &Engine, id: &str, result: Result<Value>) -> Result<()> {
    let c = e.db.connection()?;
    match result {
        Ok(value) => {
            let state = if value["partial"] == true {
                "partial"
            } else {
                "complete"
            };
            c.execute("INSERT INTO ci_source_state(source_key,cursor) SELECT source_key,?2 FROM ci_syncs WHERE id=?1 AND json_extract(request_json,'$.operation')='sync' ON CONFLICT(source_key) DO UPDATE SET cursor=excluded.cursor",params![id,value["next_url"].as_str().map(|_|id)])?;
            c.execute(
                "UPDATE ci_syncs SET state=?2,result_json=?3,finished_at=?4,retry_at=(SELECT retry_at FROM ci_source_state WHERE source_key=ci_syncs.source_key) WHERE id=?1",
                params![id, state, value.to_string(), crate::db::now()],
            )?;
        }
        Err(error) => {
            let retry = error
                .downcast_ref::<http::FetchError>()
                .and_then(|f| f.retry_at());
            c.execute("UPDATE ci_syncs SET state='failed',error=?2,retry_at=?3,finished_at=?4 WHERE id=?1",params![id,error.to_string().chars().take(512).collect::<String>(),retry,crate::db::now()])?;
        }
    }
    Ok(())
}
fn poll(e: &Engine) -> Result<()> {
    for s in &e.current_config().ci_sources {
        if e.db.project(&s.project_id).is_err() {
            continue;
        }
        let Some(seconds) = s.poll_seconds else {
            continue;
        };
        let key = s.key()?;
        let due: bool = {
            let c = e.db.connection()?;
            let last: Option<(i64, Option<i64>, Option<String>)> = c
                .query_row(
                    "SELECT last_polled,retry_at,cursor FROM ci_source_state WHERE source_key=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            last.is_none_or(|(last, retry, _)| {
                crate::db::now() - last >= (seconds * 1000) as i64
                    && retry.is_none_or(|t| t <= crate::db::now())
            })
        };
        if due {
            let cursor =
                e.db.connection()?
                    .query_row(
                        "SELECT cursor FROM ci_source_state WHERE source_key=?1",
                        [&key],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten();
            let bucket = crate::db::now() / (seconds * 1000) as i64;
            sync(
                e,
                Sync {
                    source: s.name.clone(),
                    request_id: format!("poll-{}-{bucket}", &key[..16]),
                    provider_run_id: None,
                    cursor,
                    max_runs: 1,
                    max_pages: 1,
                },
            )?;
            e.db.connection()?.execute("INSERT INTO ci_source_state(source_key,last_polled) VALUES (?1,?2) ON CONFLICT(source_key) DO UPDATE SET last_polled=excluded.last_polled",params![key,crate::db::now()])?;
        }
    }
    Ok(())
}
