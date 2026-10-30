use super::{
    artifacts,
    http::{FetchError, Http},
    store, *,
};
use crate::model::{RunState, TestCase, TestFailure, TestReport};
use reqwest::Url;
#[derive(Clone)]
pub(super) struct RemoteRun {
    pub id: String,
    pub revision: String,
    pub workflow: String,
    pub name: String,
    pub state: RunState,
    pub created: i64,
    pub started: Option<i64>,
    pub finished: i64,
    pub sha: Option<String>,
    pub url: Option<String>,
    pub metadata: Value,
}
#[derive(Clone)]
pub(super) struct RemoteJob {
    pub id: String,
    pub stage: String,
    pub state: RunState,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    pub reason: Option<String>,
    pub metadata: Value,
}
#[derive(Clone)]
pub(super) struct RemoteArtifact {
    pub id: String,
    pub stage: String,
    pub name: String,
    pub expired: bool,
    pub metadata: Value,
}
pub(super) fn number(raw: &str) -> Result<()> {
    ensure!(
        !raw.is_empty()
            && raw.len() <= 20
            && raw.bytes().all(|b| b.is_ascii_digit())
            && raw.parse::<u64>().is_ok_and(|v| v > 0),
        invalid("provider ID must be a positive decimal integer")
    );
    Ok(())
}
fn number_value(v: &Value) -> Result<String> {
    let raw = v
        .as_u64()
        .map(|v| v.to_string())
        .or_else(|| v.as_str().map(str::to_owned))
        .ok_or_else(|| invalid("provider result has no numeric ID"))?;
    number(&raw)?;
    Ok(raw)
}
fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(512).collect()
}
fn timestamp(v: &Value) -> Option<i64> {
    v.as_str()
        .and_then(|v| v.parse::<jiff::Timestamp>().ok())
        .map(|v| v.as_millisecond())
}
fn web(s: &Source, v: &Value) -> Option<String> {
    let mut url = Url::parse(v.as_str()?).ok()?;
    let base = s.base().ok()?;
    let expected = if base.host_str() == Some("api.github.com") {
        Some("github.com")
    } else {
        base.host_str()
    };
    if url.host_str() != expected
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.scheme() == "https" || s.insecure_allowed(&url))
    {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}
fn state(provider: Provider, status: &str) -> Option<RunState> {
    match (provider, status) {
        (Provider::Github, "success") | (Provider::Gitlab, "success") => Some(RunState::Succeeded),
        (_, "failure") | (_, "failed") => Some(RunState::Failed),
        (_, "cancelled") | (_, "canceled") => Some(RunState::Cancelled),
        (_, "timed_out") => Some(RunState::TimedOut),
        (_, "skipped") | (_, "neutral") => Some(RunState::Skipped),
        (_, "action_required") | (_, "stale") | (_, "startup_failure") => {
            Some(RunState::InfrastructureError)
        }
        _ => None,
    }
}
fn run(s: &Source, h: &Http, v: &Value) -> Result<Option<RemoteRun>> {
    let completed = match s.provider {
        Provider::Github => v["status"] == "completed",
        Provider::Gitlab => matches!(
            v["status"].as_str(),
            Some("success" | "failed" | "canceled" | "skipped")
        ),
    };
    if !completed {
        return Ok(None);
    }
    let provider_status = match s.provider {
        Provider::Github => text(&v["conclusion"]),
        Provider::Gitlab => text(&v["status"]),
    };
    let status = state(s.provider, &provider_status)
        .ok_or_else(|| invalid("unsupported terminal CI conclusion; no local outcome invented"))?;
    let id = number_value(&v["id"])?;
    let created = timestamp(&v["created_at"])
        .ok_or_else(|| invalid("provider run has no valid created_at timestamp"))?;
    let finished = timestamp(&v["finished_at"])
        .or_else(|| timestamp(&v["updated_at"]))
        .ok_or_else(|| invalid("provider run has no valid completion timestamp"))?;
    let started = timestamp(&v["run_started_at"]).or_else(|| timestamp(&v["started_at"]));
    let (revision, workflow, sha, url) = match s.provider {
        Provider::Github => (
            number_value(&v["run_attempt"])?,
            number_value(&v["workflow_id"])?,
            v["head_sha"].as_str(),
            web(s, &v["html_url"]),
        ),
        Provider::Gitlab => (
            format!("updated:{}", text(&v["updated_at"])),
            text(&v["source"]),
            v["sha"].as_str(),
            web(s, &v["web_url"]),
        ),
    };
    let sha = sha
        .filter(|v| matches!(v.len(), 40 | 64) && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|v| v.to_ascii_lowercase());
    let name = h.redact(&text(&v["name"]));
    let name = if name.is_empty() {
        format!(
            "{} · {}",
            s.repository,
            if workflow.is_empty() {
                "pipeline"
            } else {
                &workflow
            }
        )
    } else {
        name
    };
    let metadata = json!({"provider_status":provider_status,"provider_run_id":id,"revision":revision,"git_sha":sha,"name":name,"number":v["run_number"].as_u64().or(v["iid"].as_u64()),"ref":h.redact(&text(&v["head_branch"].as_str().map(Value::from).unwrap_or_else(||v["ref"].clone()))),"web_url":url,"source":s.name,"repository":s.repository});
    Ok(Some(RemoteRun {
        id,
        revision,
        workflow,
        name,
        state: status,
        created,
        started,
        finished,
        sha,
        url,
        metadata,
    }))
}
fn job(s: &Source, h: &Http, v: &Value) -> Result<RemoteJob> {
    let id = number_value(&v["id"])?;
    let status = match s.provider {
        Provider::Github => {
            if v["status"] == "completed" {
                text(&v["conclusion"])
            } else {
                text(&v["status"])
            }
        }
        Provider::Gitlab => text(&v["status"]),
    };
    let mapped = state(s.provider, &status).unwrap_or(RunState::Blocked);
    let started = timestamp(&v["started_at"]);
    let finished = timestamp(&v["completed_at"]).or_else(|| timestamp(&v["finished_at"]));
    let url = match s.provider {
        Provider::Github => web(s, &v["html_url"]),
        Provider::Gitlab => web(s, &v["web_url"]),
    };
    let reason = if mapped == RunState::Blocked {
        Some("provider job did not establish a completed execution".into())
    } else {
        None
    };
    Ok(RemoteJob {
        id: id.clone(),
        stage: format!("job-{id}"),
        state: mapped,
        started,
        finished,
        reason,
        metadata: json!({"name":h.redact(&text(&v["name"])),"provider_job_id":id,"provider_status":status,"web_url":url,"role":"job","provider_stage":h.redact(&text(&v["stage"])),"started_at":started,"finished_at":finished}),
    })
}
fn artifact(s: &Source, h: &Http, v: &Value) -> Result<RemoteArtifact> {
    match s.provider {
        Provider::Github => {
            let id = number_value(&v["id"])?;
            let name = h.redact(&text(&v["name"]));
            let expired = v["expired"] == true;
            Ok(RemoteArtifact {
                id: id.clone(),
                stage: "ci-summary".into(),
                name,
                expired,
                metadata: json!({"provider_artifact_id":id,"size":v["size_in_bytes"].as_u64(),"expired":expired,"digest":text(&v["digest"]),"created_at":timestamp(&v["created_at"]),"expires_at":timestamp(&v["expires_at"])}),
            })
        }
        Provider::Gitlab => {
            let id = number_value(&v["id"])?;
            Ok(RemoteArtifact {
                id: format!("job-{id}"),
                stage: format!("job-{id}"),
                name: h.redact(&text(&v["artifacts_file"]["filename"])),
                expired: false,
                metadata: json!({"provider_job_id":id,"size":v["artifacts_file"]["size"].as_u64(),"expires_at":timestamp(&v["artifacts_expire_at"]),"expired":false}),
            })
        }
    }
}
fn issue(component: &str, error: &anyhow::Error) -> Value {
    json!({"component":component,"reason":error.to_string().chars().take(256).collect::<String>()})
}
pub(super) async fn execute(
    e: &Arc<Engine>,
    s: &Source,
    sync_id: &str,
    work: Work,
) -> Result<Value> {
    let configured = source(e, &s.name)?;
    ensure!(
        configured.key()? == s.key()?,
        invalid("CI source identity changed before dispatch; submit explicitly")
    );
    let s = &configured;
    let retry: Option<i64> =
        e.db.connection()?
            .query_row(
                "SELECT retry_at FROM ci_source_state WHERE source_key=?1",
                [s.key()?],
                |v| v.get(0),
            )
            .optional()?
            .flatten();
    if let Some(at) = retry.filter(|v| *v > crate::db::now()) {
        return Err(FetchError::RateLimited(at).into());
    }
    let mut h = Http::new(s)?;
    let result = match work {
        Work::Sync(r) => sync(e, s, sync_id, &mut h, &r).await,
        Work::Artifact(r) => artifacts::fetch(e, s, &mut h, &r).await,
    };
    if let Some(at) = h.rate_at {
        e.db.connection()?.execute("INSERT INTO ci_source_state(source_key,retry_at) VALUES (?1,?2) ON CONFLICT(source_key) DO UPDATE SET retry_at=excluded.retry_at",params![s.key()?,at])?;
    }
    result
}
async fn sync(e: &Arc<Engine>, s: &Source, sync_id: &str, h: &mut Http, r: &Sync) -> Result<Value> {
    let list_url = match s.provider {
        Provider::Github => {
            let mut url = Http::page(h.github(&["actions", "runs"])?);
            url.query_pairs_mut().append_pair("status", "completed");
            url
        }
        Provider::Gitlab => Http::page(h.gitlab(&["pipelines"])?),
    };
    let mut next = None;
    let mut next_skip = 0usize;
    let mut remote = Vec::new();
    let mut issues = Vec::new();
    if let Some(provider) = &r.provider_run_id {
        let url = match s.provider {
            Provider::Github => h.github(&["actions", "runs", provider])?,
            Provider::Gitlab => h.gitlab(&["pipelines", provider])?,
        };
        let (v, _) = h.json(url).await?;
        remote.push(v);
    } else {
        let pending: Vec<String> = {
            let c = e.db.connection()?;
            c.prepare("SELECT provider_run_id FROM ci_runs WHERE source_key=?1 AND json_extract(progress_json,'$.resumable')=1 ORDER BY updated_at LIMIT ?2")?.query_map(params![s.key()?,r.max_runs],|v|v.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if r.cursor.is_none() && !pending.is_empty() {
            for provider in pending {
                let url = match s.provider {
                    Provider::Github => h.github(&["actions", "runs", &provider])?,
                    Provider::Gitlab => h.gitlab(&["pipelines", &provider])?,
                };
                remote.push(h.json(url).await?.0)
            }
        } else {
            let (mut url, mut skip) = if let Some(cursor) = &r.cursor {
                let c = e.db.connection()?;
                let old: Option<String> = c
                    .query_row(
                        "SELECT result_json FROM ci_syncs WHERE id=?1 AND source_key=?2",
                        params![cursor, s.key()?],
                        |v| v.get(0),
                    )
                    .optional()?
                    .flatten();
                let old: Value = serde_json::from_str(&old.ok_or(Fault::NotFound)?)?;
                let raw = old["next_url"].as_str().ok_or(Fault::NotFound)?;
                let skip = old["next_skip"].as_u64().unwrap_or(0) as usize;
                ensure!(skip <= 100, invalid("invalid CI pagination offset"));
                (h.cursor_url(raw, &list_url)?, skip)
            } else {
                (list_url.clone(), 0)
            };
            for _ in 0..r.max_pages {
                let (v, body) = h.json(url.clone()).await?;
                let values = match s.provider {
                    Provider::Github => v["workflow_runs"].as_array(),
                    Provider::Gitlab => v.as_array(),
                }
                .ok_or_else(|| invalid("provider run list has unexpected shape"))?;
                next = body.next.clone();
                next_skip = 0;
                for (index, value) in values.iter().enumerate().skip(skip) {
                    let completed = match s.provider {
                        Provider::Github => value["status"] == "completed",
                        Provider::Gitlab => matches!(
                            value["status"].as_str(),
                            Some("success" | "failed" | "canceled" | "skipped")
                        ),
                    };
                    if !completed {
                        continue;
                    }
                    let identity = number_value(&value["id"])?;
                    let revision = match s.provider {
                        Provider::Github => number_value(&value["run_attempt"])?,
                        Provider::Gitlab => format!("updated:{}", text(&value["updated_at"])),
                    };
                    let exists:bool=e.db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM ci_runs WHERE source_key=?1 AND provider_run_id=?2 AND provider_revision=?3)",params![s.key()?,identity,revision],|v|v.get(0))?;
                    if exists {
                        continue;
                    }
                    let detail = match s.provider {
                        Provider::Github => value.clone(),
                        Provider::Gitlab => h.json(h.gitlab(&["pipelines", &identity])?).await?.0,
                    };
                    remote.push(detail);
                    if remote.len() >= r.max_runs {
                        if index + 1 < values.len() {
                            next = Some(url.clone());
                            next_skip = index + 1;
                        }
                        break;
                    }
                }
                if remote.len() >= r.max_runs || next.is_none() {
                    break;
                }
                url = next.clone().unwrap();
                skip = 0;
            }
        }
    }
    let mut imported = Vec::new();
    let mut skipped = 0;
    for value in remote {
        let Some(remote) = run(s, h, &value)? else {
            skipped += 1;
            continue;
        };
        let local = store::ensure_run(e, s, &remote, sync_id)?;
        if h.remaining == 0 || h.rate_at.is_some() {
            imported.push(json!({"run_id":local,"partial":true,"continuation":"ci_sync with provider_run_id"}));
            continue;
        }
        match import(e, s, sync_id, h, &remote, r).await {
            Ok(value) => imported.push(value),
            Err(error) => {
                let value = issue("run", &error);
                issues.push(value.clone());
                let mut progress = store::progress(e, &local)?;
                progress["resumable"] = json!(matches!(
                    error.downcast_ref::<FetchError>(),
                    Some(FetchError::RateLimited(_) | FetchError::Network | FetchError::Budget)
                ));
                store::finish_run(e, &local, &progress, &[value])?;
                imported.push(json!({"run_id":local,"partial":true}));
            }
        }
    }
    let partial = !issues.is_empty() || imported.iter().any(|v| v["partial"] == true);
    Ok(
        json!({"runs":imported,"skipped_active":skipped,"partial":partial,"issues":issues,"next_cursor":next.as_ref().map(|_|sync_id),"next_url":next.map(|u|u.to_string()),"next_skip":next_skip,"requests_remaining":h.remaining,"provider_mutations":0}),
    )
}
async fn import(
    e: &Engine,
    s: &Source,
    sync_id: &str,
    h: &mut Http,
    remote: &RemoteRun,
    r: &Sync,
) -> Result<Value> {
    let local = store::ensure_run(e, s, remote, sync_id)?;
    let _lease = store::lease(e, &local)?;
    let mut progress = store::progress(e, &local)?;
    let mut issues = Vec::new();
    let mut resumable = false;
    let jobs_url = match s.provider {
        Provider::Github => Http::page(h.github(&[
            "actions",
            "runs",
            &remote.id,
            "attempts",
            &remote.revision,
            "jobs",
        ])?),
        Provider::Gitlab => {
            let mut url = Http::page(h.gitlab(&["pipelines", &remote.id, "jobs"])?);
            url.query_pairs_mut().append_pair("include_retried", "true");
            url
        }
    };
    if progress["jobs_complete"] != true {
        let mut url = if let Some(next) = progress["jobs_next"].as_str() {
            h.cursor_url(next, &jobs_url)?
        } else {
            jobs_url.clone()
        };
        for _ in 0..r.max_pages {
            if h.remaining < 2 {
                resumable = true;
                break;
            }
            match h.json(url.clone()).await {
                Ok((v, body)) => {
                    let rows = match s.provider {
                        Provider::Github => v["jobs"].as_array(),
                        Provider::Gitlab => v.as_array(),
                    }
                    .ok_or_else(|| invalid("provider job list has unexpected shape"))?;
                    for value in rows {
                        let job = job(s, h, value)?;
                        store::job(e, &local, &job)?;
                        if matches!(s.provider, Provider::Gitlab)
                            && value["artifacts_file"]["filename"]
                                .as_str()
                                .is_some_and(|v| !v.is_empty())
                        {
                            store::artifact(e, &local, &artifact(s, h, value)?)?;
                        }
                    }
                    store::update_definition(&*e.db.connection()?, &local)?;
                    progress["jobs_next"] = json!(body.next.as_ref().map(Url::as_str));
                    progress["jobs_complete"] = json!(body.next.is_none());
                    store::save_progress(e, &local, &progress)?;
                    let Some(next) = body.next else { break };
                    url = next;
                    resumable = true;
                }
                Err(error) => {
                    issues.push(issue("jobs", &error));
                    resumable = true;
                    break;
                }
            }
        }
    }
    let expired = crate::evidence_expiry::expired(
        &*e.db.connection()?,
        &local,
        crate::evidence_expiry::Category::Logs,
    )?;
    for (stage, job, offset) in store::pending_logs(e, &local, r.provider_run_id.is_some())? {
        if expired {
            store::log_state(e, &local, &stage, "local_expired")?;
            continue;
        }
        if h.remaining < 2 {
            resumable = true;
            break;
        }
        if offset >= 32 * 1024 * 1024 {
            issues.push(json!({"component":"logs","stage_id":stage,"reason":"32 MiB per-job import ceiling reached"}));
            store::log_state(e, &local, &stage, "unavailable")?;
            continue;
        }
        let url = match s.provider {
            Provider::Github => h.github(&["actions", "jobs", &job, "logs"])?,
            Provider::Gitlab => h.gitlab(&["jobs", &job, "trace"])?,
        };
        match h.get(url, 4 * 1024 * 1024, Some(offset), None).await {
            Ok(mut body) => {
                if !body.complete {
                    if let Some(end) = body.bytes.iter().rposition(|b| *b == b'\n') {
                        body.bytes.truncate(end + 1)
                    } else {
                        store::log_state(e, &local, &stage, "unavailable")?;
                        issues.push(json!({"component":"logs","stage_id":stage,"reason":"log line exceeds bounded import window"}));
                        continue;
                    }
                }
                let raw = std::str::from_utf8(&body.bytes)
                    .map_err(|_| invalid("provider log is not UTF-8; no lossy text imported"))?;
                let text = h.redact(raw);
                store::append_log(e, &local, &stage, offset, &body.bytes, &text, body.complete)?;
                resumable |= !body.complete;
            }
            Err(error) => {
                let retryable = matches!(
                    error.downcast_ref::<FetchError>(),
                    Some(FetchError::RateLimited(_) | FetchError::Network | FetchError::Budget)
                );
                if retryable {
                    resumable = true
                } else {
                    store::log_state(e, &local, &stage, "unavailable")?;
                }
                issues.push(issue("logs", &error));
                if h.rate_at.is_some() {
                    break;
                }
            }
        }
    }
    if matches!(s.provider, Provider::Github)
        && progress["artifacts_complete"] != true
        && h.remaining > 0
    {
        let base = Http::page(h.github(&["actions", "runs", &remote.id, "artifacts"])?);
        let mut url = if let Some(next) = progress["artifacts_next"].as_str() {
            h.cursor_url(next, &base)?
        } else {
            base.clone()
        };
        for _ in 0..r.max_pages {
            if h.remaining == 0 {
                resumable = true;
                break;
            }
            match h.json(url.clone()).await {
                Ok((v, body)) => {
                    let rows = v["artifacts"]
                        .as_array()
                        .ok_or_else(|| invalid("provider artifact list has unexpected shape"))?;
                    for value in rows {
                        store::artifact(e, &local, &artifact(s, h, value)?)?;
                    }
                    progress["artifacts_next"] = json!(body.next.as_ref().map(Url::as_str));
                    progress["artifacts_complete"] = json!(body.next.is_none());
                    store::save_progress(e, &local, &progress)?;
                    let Some(next) = body.next else { break };
                    url = next;
                    resumable = true;
                }
                Err(error) => {
                    issues.push(issue("artifact_metadata", &error));
                    resumable = true;
                    break;
                }
            }
        }
    } else if matches!(s.provider, Provider::Gitlab) {
        progress["artifacts_complete"] = progress["jobs_complete"].clone();
    }
    if r.provider_run_id.is_some() && progress["tests_unavailable"] == true {
        progress["tests_checked"] = false.into();
        progress["tests_next"] = Value::Null;
        progress["tests_page"] = json!(0);
    }
    if progress["tests_checked"] != true && h.remaining > 0 {
        import_tests(
            e,
            s,
            h,
            remote,
            &local,
            &mut progress,
            r.max_pages,
            &mut issues,
        )
        .await?;
    }
    let pending:bool=e.db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM ci_jobs WHERE run_id=?1 AND log_state IN ('pending','partial'))",[&local],|v|v.get(0))?;
    progress["resumable"] = json!(
        pending
            || progress["jobs_complete"] != true
            || progress["artifacts_complete"] != true
            || progress["tests_checked"] != true
    );
    let _ = resumable;
    store::finish_run(e, &local, &progress, &issues)
}
#[allow(clippy::too_many_arguments)] // Shared import context and mutable coverage stay explicit.
async fn import_tests(
    e: &Engine,
    s: &Source,
    h: &mut Http,
    remote: &RemoteRun,
    local: &str,
    progress: &mut Value,
    max_pages: usize,
    issues: &mut Vec<Value>,
) -> Result<()> {
    match s.provider {
        Provider::Gitlab => {
            let base = h.gitlab(&["pipelines", &remote.id, "test_report"])?;
            let mut url = if let Some(next) = progress["tests_next"].as_str() {
                h.cursor_url(next, &base)?
            } else {
                base.clone()
            };
            for _ in 0..max_pages {
                if h.remaining == 0 {
                    break;
                }
                match h.json(url.clone()).await {
                    Ok((value, body)) => {
                        let page = progress["tests_page"].as_u64().unwrap_or(0);
                        ensure!(
                            page < 100,
                            invalid("CI test-report pagination exceeds 100 pages")
                        );
                        if let Some(mut report) = gitlab_report(h, &value, true)? {
                            for case in &mut report.cases {
                                case.id = format!("page-{page}:{}", case.id);
                            }
                            e.db.connection()?.execute("INSERT INTO ci_test_pages VALUES (?1,?2,?3) ON CONFLICT(run_id,page_key) DO UPDATE SET report_json=excluded.report_json",params![local,format!("page-{page}"),serde_json::to_string(&report)?])?;
                            if let Some(counts) = declared_counts(&value) {
                                if progress["test_counts"].is_object() {
                                    ensure!(
                                        progress["test_counts"] == serde_json::to_value(&counts)?,
                                        invalid("provider test totals changed during import")
                                    );
                                }
                                progress["test_counts"] = serde_json::to_value(counts)?;
                            }
                        }
                        progress["tests_page"] = json!(page + 1);
                        progress["tests_next"] = json!(body.next.as_ref().map(Url::as_str));
                        progress["tests_checked"] = json!(body.next.is_none());
                        progress["tests_unavailable"] = false.into();
                        progress["test_issue"] = Value::Null;
                        store::save_progress(e, local, progress)?;
                        let Some(next) = body.next else { break };
                        url = next;
                    }
                    Err(error) => {
                        issues.push(issue("tests", &error));
                        let permanent = matches!(
                            error.downcast_ref::<FetchError>(),
                            Some(FetchError::Http(403) | FetchError::Http(404))
                        );
                        progress["tests_checked"] = json!(permanent);
                        progress["tests_unavailable"] = json!(permanent);
                        progress["test_issue"] =
                            json!(error.to_string().chars().take(256).collect::<String>());
                        break;
                    }
                }
            }
            let pages: Vec<String> = {
                let c = e.db.connection()?;
                c.prepare("SELECT report_json FROM ci_test_pages WHERE run_id=?1 ORDER BY CAST(substr(page_key,6) AS INTEGER) LIMIT 100")?.query_map([local],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if !pages.is_empty() {
                let mut report = TestReport::default();
                for page in pages {
                    report.merge(serde_json::from_str(&page)?)
                }
                let mut occurrences = std::collections::BTreeMap::new();
                for case in &mut report.cases {
                    let key = serde_json::to_string(&(&case.suite, &case.class_name, &case.name))?;
                    let occurrence = occurrences.entry(key.clone()).or_insert(0usize);
                    case.id = crate::hashing::hex(Sha256::digest(format!("{key}:{occurrence}")));
                    *occurrence += 1;
                }
                let declared = progress["test_counts"].is_object();
                if declared {
                    report.counts = serde_json::from_value(progress["test_counts"].clone())?;
                }
                report.cases_complete &= progress["tests_checked"] == true
                    && !report.truncated
                    && report.counts.passed + report.counts.failed + report.counts.skipped
                        == report.cases.len() as u64;
                if declared || progress["tests_checked"] == true {
                    store::tests(e, local, report)?;
                }
            }
        }
        Provider::Github => {
            let mut report = TestReport::default();
            let mut found = false;
            let mut parsed = 0;
            let mut transient = false;
            let mut unavailable = false;
            let artifacts = store::selected_artifacts(e, local)?;
            let mut artifact_names = std::collections::BTreeSet::new();
            for artifact in artifacts {
                if !s.test_artifacts.contains(&artifact.name) {
                    continue;
                }
                found = true;
                let created = artifact.metadata["created_at"].as_i64();
                if !created.is_some_and(|t| {
                    remote.started.is_some_and(|start| t >= start) && t <= remote.finished
                }) || !artifact_names.insert(artifact.name.clone())
                {
                    unavailable = true;
                    progress["test_issue"] = json!(
                        "test artifact attempt scope is unavailable or artifact name is ambiguous"
                    );
                    continue;
                }
                let result = match artifacts::download(e, s, h, local, &artifact).await {
                    Ok(path) => artifacts::junit(h, &path, &artifact.name),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(reports) => {
                        parsed += reports.len();
                        for value in reports {
                            report.merge(value)
                        }
                    }
                    Err(error) => {
                        issues.push(issue("tests", &error));
                        let retry = matches!(
                            error.downcast_ref::<FetchError>(),
                            Some(
                                FetchError::RateLimited(_)
                                    | FetchError::Network
                                    | FetchError::Budget
                            )
                        );
                        transient |= retry;
                        unavailable |= !retry;
                        progress["test_issue"] =
                            json!(error.to_string().chars().take(256).collect::<String>());
                    }
                }
            }
            if parsed > 0 {
                report.cases_complete &= !transient && !unavailable;
                store::tests(e, local, report)?;
            }
            let missing =
                !s.test_artifacts.is_empty() && !found && progress["artifacts_complete"] == true;
            if missing {
                progress["test_issue"] =
                    json!("configured test-report artifacts are not published");
            }
            progress["tests_unavailable"] = json!(unavailable || missing);
            progress["tests_checked"] = json!(!transient && progress["artifacts_complete"] == true);
        }
    }
    Ok(())
}
fn declared_counts(v: &Value) -> Option<crate::model::TestCounts> {
    let total = v["total_count"].as_u64()?;
    let passed = v["success_count"].as_u64()?;
    let failed = v["failed_count"]
        .as_u64()?
        .checked_add(v["error_count"].as_u64().unwrap_or(0))?;
    let skipped = v["skipped_count"].as_u64()?;
    if total > 100000000 || passed.checked_add(failed)?.checked_add(skipped)? != total {
        return None;
    }
    Some(crate::model::TestCounts {
        passed,
        failed,
        skipped,
    })
}
fn gitlab_report(h: &Http, v: &Value, complete: bool) -> Result<Option<TestReport>> {
    let suites = v["test_suites"]
        .as_array()
        .ok_or_else(|| invalid("GitLab test report shape unavailable"))?;
    if suites.is_empty() {
        return Ok(None);
    }
    let mut report = TestReport {
        cases_complete: complete,
        ..Default::default()
    };
    for suite in suites {
        for case in suite["test_cases"].as_array().into_iter().flatten() {
            if report.cases.len() >= 10000 {
                report.cases_complete = false;
                report.truncated = true;
                break;
            }
            let status = match case["status"].as_str() {
                Some("success") => "passed",
                Some("skipped") => "skipped",
                Some("failed" | "error") => "failed",
                _ => {
                    report.cases_complete = false;
                    "unknown"
                }
            };
            match status {
                "passed" => report.counts.passed += 1,
                "failed" => report.counts.failed += 1,
                "skipped" => report.counts.skipped += 1,
                _ => {}
            }
            let name = h.redact(&text(&case["name"]));
            let class = h.redact(&text(&case["classname"]));
            let suite = h.redact(&text(&suite["name"]));
            let message = h.redact(&text(&case["stack_trace"]));
            let identity = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&json!([
                suite,
                class,
                name,
                report.cases.len()
            ]))?));
            if status == "failed" && report.failures.len() < 20 {
                report.failures.push(TestFailure {
                    name: name.clone(),
                    class_name: class.clone(),
                    message: message.clone(),
                })
            }
            report.cases.push(TestCase {
                id: identity,
                name,
                class_name: class,
                suite,
                status: status.into(),
                message,
                file: case["file"].as_str().map(|v| h.redact(v)),
                line: case["line"].as_u64(),
                duration_seconds: case["execution_time"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.),
                output: case["system_output"]
                    .as_str()
                    .map(|v| crate::model::TestOutput {
                        stdout: h.redact(&v.chars().take(16384).collect::<String>()),
                        stderr: String::new(),
                        truncated: v.chars().count() > 16384,
                    }),
            });
        }
    }
    Ok(Some(report))
}
