#[allow(dead_code)]
mod common;
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use preflight::{ci, engine::Engine};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
#[derive(Clone, Copy)]
enum Provider {
    Github,
    Gitlab,
}
struct Mock {
    provider: Provider,
    base: String,
    requests: Mutex<Vec<String>>,
    rate: AtomicBool,
    slow: AtomicBool,
    unscoped: AtomicBool,
    revision: AtomicU64,
    no_reports: bool,
    archive: Vec<u8>,
    archive_sha: String,
}
fn zipped(path: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(b"<testsuite tests='2' failures='1'><testcase name='ok' classname='sample'><system-out>case marker</system-out></testcase><testcase name='bad' classname='sample'><failure>expected failure</failure></testcase></testsuite>").unwrap();
    writer.finish().unwrap().into_inner()
}
fn gh_run(id: u64, attempt: u64) -> Value {
    json!({"id":id,"run_attempt":attempt,"workflow_id":7,"run_number":id,"name":"Build and test","status":"completed","conclusion":"failure","head_sha":"a".repeat(40),"head_branch":"main","created_at":"2026-10-20T10:00:00Z","run_started_at":"2026-10-20T10:00:01Z","updated_at":"2026-10-20T10:02:00Z","html_url":format!("https://github.com/org/repo/actions/runs/{id}")})
}
fn gl_run(revision: u64) -> Value {
    json!({"id":100,"iid":3,"name":"Verify","status":if revision==1{"failed"}else{"success"},"sha":"b".repeat(40),"ref":"main","source":"push","created_at":"2026-10-20T10:00:00Z","started_at":"2026-10-20T10:00:01Z","finished_at":format!("2026-10-20T10:0{}:00Z",revision+1),"updated_at":format!("2026-10-20T10:0{}:00Z",revision+1),"web_url":"https://gitlab.com/group/repo/-/pipelines/100"})
}
fn response(v: Value) -> Response {
    axum::Json(v).into_response()
}
async fn handler(State(m): State<Arc<Mock>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if m.slow.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    let query = request.uri().query().unwrap_or("").to_owned();
    m.requests
        .lock()
        .unwrap()
        .push(format!("{} {path}?{query}", request.method()));
    if request.method() != axum::http::Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let revision = m.revision.load(Ordering::SeqCst);
    if path.ends_with("/trace") || path.contains("/actions/jobs/") && path.ends_with("/logs") {
        return (
            [(header::CONTENT_TYPE, "text/plain")],
            format!(
                "error: fixture compiler problem\nAuthorization: Bearer ghp_{}\n",
                "a".repeat(36)
            ),
        )
            .into_response();
    }
    if path.ends_with("/zip") || path.ends_with("/artifacts") && path.contains("/jobs/") {
        if path.contains("/56/") {
            return StatusCode::GONE.into_response();
        }
        return (
            [(header::CONTENT_TYPE, "application/zip")],
            m.archive.clone(),
        )
            .into_response();
    }
    match m.provider {
        Provider::Github => {
            if path == "/repos/org/repo/actions/runs" {
                return response(json!({"workflow_runs":[gh_run(100,revision),gh_run(101,1)]}));
            }
            if path == "/repos/org/repo/actions/runs/100" {
                return response(gh_run(100, revision));
            }
            if path == "/repos/org/repo/actions/runs/101" {
                return response(gh_run(101, 1));
            }
            if path.ends_with("/jobs") {
                if m.rate.load(Ordering::SeqCst) {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(header::RETRY_AFTER, "3600")],
                        "retry later",
                    )
                        .into_response();
                }
                let second = query.contains("page=2");
                let job = if second { 12 } else { 11 };
                let v = json!({"jobs":[{"id":job,"name":if second{"Unit tests"}else{"Compile"},"status":"completed","conclusion":"failure","started_at":"2026-10-20T10:00:01Z","completed_at":"2026-10-20T10:01:00Z","html_url":format!("https://github.com/org/repo/actions/runs/100/job/{job}")}]});
                if !second {
                    let link = format!("<{}{}?per_page=100&page=2>; rel=\"next\"", m.base, path);
                    return ([(header::LINK, link)], axum::Json(v)).into_response();
                }
                return response(v);
            }
            if path.ends_with("/artifacts") {
                return response(
                    json!({"artifacts":if m.no_reports{json!([])}else{json!([{"id":55+100*(m.revision.load(Ordering::SeqCst)-1),"name":"junit","created_at":if m.unscoped.load(Ordering::SeqCst){Value::Null}else{json!("2026-10-20T10:01:30Z")},"expired":false,"size_in_bytes":m.archive.len(),"digest":format!("sha256:{}",m.archive_sha)},{"id":56,"name":"old","expired":true,"size_in_bytes":100}])}}),
                );
            }
        }
        Provider::Gitlab => {
            if path == "/projects/group%2Frepo/pipelines" {
                return response(json!([gl_run(revision)]));
            }
            if path == "/projects/group%2Frepo/pipelines/100" {
                return response(gl_run(revision));
            }
            if path.ends_with("/jobs") {
                return response(
                    json!([{"id":21,"name":"Compile","stage":"build","status":"success","started_at":"2026-10-20T10:00:01Z","finished_at":"2026-10-20T10:01:00Z"},{"id":22,"name":"Unit tests","stage":"test","status":"failed","artifacts_file":{"filename":"test-results.zip","size":m.archive.len()},"artifacts_expire_at":"2020-01-01T00:00:00Z"}]),
                );
            }
            if path.ends_with("/test_report") {
                if m.no_reports {
                    return response(json!({"total_count":0,"test_suites":[]}));
                }
                let second = query.contains("page=2");
                let body = json!({"total_count":2,"success_count":1,"failed_count":1,"error_count":0,"skipped_count":0,"test_suites":[{"name":"tests","test_cases":[{"name":"repeated name","classname":"shared","status":if second{"failed"}else{"success"},"system_output":"published case output","stack_trace":if second{"assertion failed"}else{""}}]}]});
                if !second {
                    let link = format!("<{}{}?page=2>; rel=\"next\"", m.base, path);
                    return ([(header::LINK, link)], axum::Json(body)).into_response();
                }
                return response(body);
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}
struct Fixture {
    f: common::Fixture,
    mock: Arc<Mock>,
    stop: tokio_util::sync::CancellationToken,
    server: tokio::task::JoinHandle<()>,
    worker: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(provider: Provider, no_reports: bool, bad_zip: bool) -> Self {
        use sha2::{Digest, Sha256};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let archive = zipped(if bad_zip {
            "../escape.xml"
        } else {
            "junit.xml"
        });
        let hash = preflight::hashing::hex(Sha256::digest(&archive));
        let mock = Arc::new(Mock {
            provider,
            base: base.clone(),
            requests: Mutex::new(Vec::new()),
            rate: AtomicBool::new(false),
            slow: AtomicBool::new(false),
            unscoped: AtomicBool::new(false),
            revision: AtomicU64::new(1),
            no_reports,
            archive,
            archive_sha: hash,
        });
        let stop = tokio_util::sync::CancellationToken::new();
        let signal = stop.clone();
        let app = Router::new()
            .fallback(any(handler))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
                .unwrap()
        });
        let mut f = common::Fixture::new();
        let mut config = f.engine.config.clone();
        config.ci_sources=serde_json::from_value(json!([{"name":"remote","project_id":f.project.id,"provider":match provider{Provider::Github=>"github",Provider::Gitlab=>"gitlab"},"repository":match provider{Provider::Github=>"org/repo",Provider::Gitlab=>"group/repo"},"api_base":base,"allow_insecure_loopback":true,"test_artifacts":if no_reports{vec![]}else{vec!["junit"]}}])).unwrap();
        f.engine.shutdown().await.unwrap();
        drop(f.engine);
        f.engine = Engine::open(config).unwrap();
        let worker = tokio::spawn(ci::run(f.engine.clone()));
        Self {
            f,
            mock,
            stop,
            server,
            worker,
        }
    }
    async fn sync(&self, provider: Option<&str>, cursor: Option<&str>, pages: usize) -> Value {
        let queued=ci::sync(&self.f.engine,serde_json::from_value(json!({"source":"remote","request_id":uuid::Uuid::now_v7().to_string(),"provider_run_id":provider,"cursor":cursor,"max_runs":1,"max_pages":pages})).unwrap()).unwrap();
        self.wait(&queued).await
    }
    async fn wait(&self, queued: &Value) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let status = ci::sync_status(
                    &self.f.engine,
                    &serde_json::from_value(
                        json!({"project_id":self.f.project.id,"sync_id":queued["sync_id"]}),
                    )
                    .unwrap(),
                )
                .unwrap();
                if matches!(
                    status["state"].as_str(),
                    Some("complete" | "partial" | "failed" | "cancelled")
                ) {
                    return status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn close(self) {
        self.f.engine.shutdown().await.unwrap();
        self.worker.await.unwrap();
        self.stop.cancel();
        self.server.await.unwrap();
    }
}
fn details(e: &Engine, project: &str, run: &str) -> Value {
    ci::get(
        e,
        &serde_json::from_value(
            json!({"project_id":project,"run_id":run,"max_bytes":32768,"limit":50}),
        )
        .unwrap(),
    )
    .unwrap()
}
fn imported(status: &Value) -> String {
    assert_ne!(status["state"], "failed", "{status}");
    status["result"]["runs"][0]["run_id"]
        .as_str()
        .unwrap()
        .into()
}
#[tokio::test]
async fn github_completed_attempts_jobs_logs_junit_artifacts_and_idempotency_are_real_http() {
    let f = Fixture::new(Provider::Github, false, false).await;
    let status = f.sync(Some("100"), None, 2).await;
    let id = imported(&status);
    let run = f.f.engine.db.run(&id).unwrap();
    assert_eq!(run.status, preflight::model::RunState::Failed);
    assert!(run.external.is_some());
    assert!(
        run.stages
            .iter()
            .any(|s| s.external.as_ref().is_some_and(|v| v["name"] == "Compile"))
    );
    let report = preflight::api::compact_report(&f.f.engine, &id).unwrap();
    assert_eq!(report["tests"]["passed"], 1);
    assert_eq!(report["tests"]["failed"], 1);
    assert!(
        run.stages
            .iter()
            .find(|s| s.definition.id == "ci-tests")
            .unwrap()
            .tests
            .as_ref()
            .unwrap()
            .cases_complete
    );
    let page = f.f.engine.db.stage_logs(&id, None, 0).unwrap();
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains("ghp_aaaaaaaa")
    );
    let detail = details(&f.f.engine, &f.f.project.id, &id);
    assert_eq!(detail["import_state"], "complete");
    assert_eq!(detail["artifacts"][0]["state"], "downloaded");
    assert_eq!(detail["artifacts"][1]["state"], "provider_expired");
    let again = f.sync(Some("100"), None, 2).await;
    assert_eq!(imported(&again), id);
    let replay = f.f.engine.db.stage_logs(&id, None, 0).unwrap();
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        serde_json::to_value(&replay).unwrap()
    );
    f.mock.revision.store(2, Ordering::SeqCst);
    let next = imported(&f.sync(Some("100"), None, 2).await);
    assert_ne!(next, id);
    let ids = |run: &str| {
        f.f.engine
            .db
            .run(run)
            .unwrap()
            .stages
            .into_iter()
            .find(|s| s.definition.id == "ci-tests")
            .unwrap()
            .tests
            .unwrap()
            .cases
            .into_iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&id),
        ids(&next),
        "case identity must not depend on artifact ID or run attempt"
    );
    assert_eq!(
        f.f.engine.db.run(&id).unwrap().status,
        preflight::model::RunState::Failed
    );
    let retry = preflight::retry::preview(
        &f.f.engine,
        &serde_json::from_value(
            json!({"project_id":f.f.project.id,"run_id":id,"stage_id":"job-11"}),
        )
        .unwrap(),
    );
    assert!(retry.unwrap_err().to_string().contains("read-only"));
    assert!(
        f.mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|v| v.starts_with("GET "))
    );
    assert_eq!(f.f.engine.scheduler_status()["reserved_slots"], 0);
    f.close().await;
}
#[tokio::test]
async fn discovery_cursor_never_skips_same_page_runs_and_partial_job_pages_resume() {
    let f = Fixture::new(Provider::Github, true, false).await;
    let first = f.sync(None, None, 1).await;
    let a = imported(&first);
    assert_eq!(first["state"], "partial");
    let cursor = first["result"]["next_cursor"].as_str().unwrap();
    assert!(
        !first["result"]
            .as_object()
            .unwrap()
            .contains_key("next_url")
    );
    let second = f.sync(None, Some(cursor), 1).await;
    let b = imported(&second);
    assert_ne!(a, b);
    let complete = f.sync(Some("100"), None, 1).await;
    assert_eq!(imported(&complete), a);
    assert_eq!(complete["state"], "complete");
    let r = preflight::api::compact_report(&f.f.engine, &a).unwrap();
    assert!(r["tests"]["passed"].is_null());
    assert_eq!(
        details(&f.f.engine, &f.f.project.id, &a)["coverage"]["structured_tests"],
        "unknown"
    );
    f.close().await;
}
#[tokio::test]
async fn gitlab_paged_published_reports_keep_case_identity_and_status_across_revisions() {
    let f = Fixture::new(Provider::Gitlab, false, false).await;
    let first = f.sync(Some("100"), None, 1).await;
    let id = imported(&first);
    let partial = f.f.engine.db.run(&id).unwrap();
    let report = partial
        .stages
        .iter()
        .find(|s| s.definition.id == "ci-tests")
        .unwrap()
        .tests
        .as_ref()
        .unwrap();
    assert!(!report.cases_complete);
    assert_eq!(report.counts.passed, 1);
    assert_eq!(report.counts.failed, 1);
    let second = f.sync(Some("100"), None, 1).await;
    assert_eq!(imported(&second), id);
    let run = f.f.engine.db.run(&id).unwrap();
    let report = run
        .stages
        .iter()
        .find(|s| s.definition.id == "ci-tests")
        .unwrap()
        .tests
        .as_ref()
        .unwrap();
    assert!(report.cases_complete);
    assert_eq!(report.cases.len(), 2);
    assert_ne!(report.cases[0].id, report.cases[1].id);
    let evidence=preflight::evidence::search(&f.f.engine,&serde_json::from_value(json!({"project_id":f.f.project.id,"run_id":id,"test_id":report.cases[0].id,"query":"published case output"})).unwrap()).unwrap();
    assert!(!evidence["matches"].as_array().unwrap().is_empty());
    f.mock.revision.store(2, Ordering::SeqCst);
    let next = imported(&f.sync(Some("100"), None, 2).await);
    assert_ne!(id, next);
    assert_eq!(
        f.f.engine.db.run(&next).unwrap().status,
        preflight::model::RunState::Succeeded
    );
    assert_eq!(
        f.f.engine.db.run(&id).unwrap().status,
        preflight::model::RunState::Failed
    );
    assert!(
        f.mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.contains("include_retried=true"))
    );
    f.close().await;
}
#[tokio::test]
async fn rate_limit_is_durable_and_retry_never_sends_early_requests() {
    let f = Fixture::new(Provider::Github, true, false).await;
    f.mock.rate.store(true, Ordering::SeqCst);
    let first = f.sync(Some("100"), None, 2).await;
    assert_eq!(first["state"], "partial");
    let count = f.mock.requests.lock().unwrap().len();
    let retry = f.sync(Some("100"), None, 2).await;
    assert_eq!(retry["state"], "failed");
    assert!(retry["retry_at"].as_i64().is_some());
    assert_eq!(count, f.mock.requests.lock().unwrap().len());
    f.mock.rate.store(false, Ordering::SeqCst);
    rusqlite::Connection::open(f.f.engine.config.data_dir.join("preflight.sqlite3"))
        .unwrap()
        .execute("UPDATE ci_source_state SET retry_at=0", [])
        .unwrap();
    let fixed = f.sync(Some("100"), None, 2).await;
    assert_eq!(fixed["state"], "complete");
    f.close().await;
}
#[tokio::test]
async fn malicious_zip_is_data_only_and_cannot_create_files_or_invent_test_counts() {
    let f = Fixture::new(Provider::Github, false, true).await;
    let status = f.sync(Some("100"), None, 2).await;
    let id = imported(&status);
    assert_eq!(status["state"], "partial");
    assert!(preflight::api::compact_report(&f.f.engine, &id).unwrap()["tests"]["passed"].is_null());
    assert!(!f.f.temp.path().join("escape.xml").exists());
    assert!(!f.f.project.root.join("escape.xml").exists());
    f.close().await;
}
#[tokio::test]
async fn selected_artifacts_are_hashed_scoped_and_expired_metadata_is_preserved() {
    let f = Fixture::new(Provider::Gitlab, true, false).await;
    let status = f.sync(Some("100"), None, 2).await;
    let id = imported(&status);
    let request = json!({"project_id":f.f.project.id,"run_id":id,"artifact_id":"job-22","request_id":"selected-download"});
    let queued = ci::artifact_fetch(
        &f.f.engine,
        serde_json::from_value(request.clone()).unwrap(),
    )
    .unwrap();
    let result = f.wait(&queued).await;
    assert_eq!(result["state"], "complete", "{result}");
    assert_eq!(result["result"]["sha256"], f.mock.archive_sha);
    assert_eq!(
        ci::artifact_fetch(
            &f.f.engine,
            serde_json::from_value(request.clone()).unwrap()
        )
        .unwrap()["sync_id"],
        queued["sync_id"]
    );
    let mut foreign = request;
    foreign["project_id"] = json!(uuid::Uuid::now_v7().to_string());
    assert!(ci::artifact_fetch(&f.f.engine, serde_json::from_value(foreign).unwrap()).is_err());
    assert!(
        ci::get(
            &f.f.engine,
            &serde_json::from_value(
                json!({"project_id":uuid::Uuid::now_v7().to_string(),"run_id":id})
            )
            .unwrap()
        )
        .is_err()
    );
    let bytes = ci::artifact_file(
        &f.f.engine,
        &serde_json::from_value(
            json!({"project_id":f.f.project.id,"run_id":id,"artifact_id":"job-22"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(bytes.0, f.mock.archive);
    f.close().await;
}

#[tokio::test]
async fn mcp_http_import_status_evidence_and_binary_download_share_contract() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = Fixture::new(Provider::Github, false, false).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = preflight::api::router(f.f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{addr}/mcp"
        )))
        .await
        .unwrap();
    let args = json!({"source":"remote","request_id":"mcp-http-import","provider_run_id":"100"});
    let output = client
        .call_tool(
            CallToolRequestParams::new("ci_sync").with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(output.is_error, Some(true));
    let queued: Value = serde_json::from_str(&output.content[0].as_text().unwrap().text).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let rest: Value = http
        .post(format!("http://{addr}/api/v1/ci/sync"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rest, queued);
    let status = f.wait(&queued).await;
    let id = imported(&status);
    let get = json!({"project_id":f.f.project.id,"run_id":id});
    let output = client
        .call_tool(
            CallToolRequestParams::new("ci_run_get")
                .with_arguments(get.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(output.is_error, Some(true));
    let text = &output.content[0].as_text().unwrap().text;
    assert!(text.len() <= 8192);
    let detail: Value = serde_json::from_str(text).unwrap();
    let rest: Value = http
        .get(format!(
            "http://{addr}/api/v1/ci/run?project_id={}&run_id={}",
            f.f.project.id, id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail, rest);
    let zip = http
        .get(format!(
            "http://{addr}/api/v1/ci/artifact-file?project_id={}&run_id={}&artifact_id=55",
            f.f.project.id, id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(zip.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(
        zip.headers()[header::ETAG],
        format!("\"{}\"", f.mock.archive_sha)
    );
    assert_eq!(
        zip.bytes().await.unwrap().as_ref(),
        f.mock.archive.as_slice()
    );
    let logs=preflight::evidence::search(&f.f.engine,&serde_json::from_value(json!({"project_id":f.f.project.id,"run_id":id,"git_sha":"a".repeat(40),"query":"fixture compiler"})).unwrap()).unwrap();
    assert!(!logs["matches"].as_array().unwrap().is_empty());
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn read_only_import_restarts_resume_same_ids_and_committed_artifact_intent() {
    let f = Fixture::new(Provider::Gitlab, true, false).await;
    let id = imported(&f.sync(Some("100"), None, 2).await);
    let request = json!({"project_id":f.f.project.id,"run_id":id,"artifact_id":"job-22","request_id":"download-before-restart"});
    let queued = ci::artifact_fetch(&f.f.engine, serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(f.wait(&queued).await["state"], "complete");
    let resume = json!({"source":"remote","request_id":"resume-import","provider_run_id":"100"});
    let queued = ci::sync(&f.f.engine, serde_json::from_value(resume.clone()).unwrap()).unwrap();
    let Fixture {
        mut f,
        mock,
        stop,
        server,
        worker,
    } = f;
    f.engine.shutdown().await.unwrap();
    worker.await.unwrap();
    let database = f.engine.config.data_dir.join("preflight.sqlite3");
    let c = rusqlite::Connection::open(&database).unwrap();
    c.execute(
        "UPDATE ci_syncs SET state='running' WHERE id=?1",
        [queued["sync_id"].as_str().unwrap()],
    )
    .unwrap();
    c.execute("DELETE FROM artifacts WHERE run_id=?1", [&id])
        .unwrap();
    c.execute("UPDATE ci_artifacts SET state='publishing',provider_json=json_set(provider_json,'$.expired',json('true')) WHERE run_id=?1",[&id]).unwrap();
    drop(c);
    let config = f.engine.config.clone();
    drop(f.engine);
    f.engine = Engine::open(config).unwrap();
    let worker = tokio::spawn(ci::run(f.engine.clone()));
    let f = Fixture {
        f,
        mock,
        stop,
        server,
        worker,
    };
    let replay = ci::sync(&f.f.engine, serde_json::from_value(resume).unwrap()).unwrap();
    assert_eq!(replay["sync_id"], queued["sync_id"]);
    assert_eq!(imported(&f.wait(&replay).await), id);
    let count = f.mock.requests.lock().unwrap().len();
    let queued=ci::artifact_fetch(&f.f.engine,serde_json::from_value(json!({"project_id":f.f.project.id,"run_id":id,"artifact_id":"job-22","request_id":"recover-publication"})).unwrap()).unwrap();
    assert_eq!(f.wait(&queued).await["state"], "complete");
    assert_eq!(count, f.mock.requests.lock().unwrap().len());
    assert_eq!(f.f.engine.db.artifacts(&id).unwrap().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn cancellation_is_local_scoped_and_survives_queued_and_running_imports() {
    let f = Fixture::new(Provider::Github, true, false).await;
    let request = |key| {
        serde_json::from_value(json!({"source":"remote","request_id":key,"provider_run_id":"100"}))
            .unwrap()
    };
    let queued = ci::sync(&f.f.engine, request("queued-stop")).unwrap();
    let cancel = |id: &Value| {
        serde_json::from_value(json!({"project_id":f.f.project.id,"sync_id":id["sync_id"],"actor":"user","reason":"test local stop"})).unwrap()
    };
    assert_eq!(
        ci::cancel(&f.f.engine, &cancel(&queued)).unwrap()["state"],
        "cancelled"
    );
    assert!(f.mock.requests.lock().unwrap().is_empty());
    f.mock.slow.store(true, Ordering::SeqCst);
    let running = ci::sync(&f.f.engine, request("running-stop")).unwrap();
    for _ in 0..100 {
        let status = ci::sync_status(
            &f.f.engine,
            &serde_json::from_value(
                json!({"project_id":f.f.project.id,"sync_id":running["sync_id"]}),
            )
            .unwrap(),
        )
        .unwrap();
        if status["state"] == "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    ci::cancel(&f.f.engine, &cancel(&running)).unwrap();
    assert_eq!(f.wait(&running).await["state"], "cancelled");
    assert!(
        f.mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|v| v.starts_with("GET "))
    );
    let foreign=serde_json::from_value(json!({"project_id":uuid::Uuid::now_v7(),"sync_id":running["sync_id"],"actor":"user","reason":"wrong project"})).unwrap();
    assert!(ci::cancel(&f.f.engine, &foreign).is_err());
    f.close().await;
}

#[tokio::test]
async fn unscoped_attempt_reports_stay_unknown_and_trashed_imports_stay_hidden() {
    let f = Fixture::new(Provider::Github, false, false).await;
    f.mock.unscoped.store(true, Ordering::SeqCst);
    let result = f.sync(Some("100"), None, 2).await;
    let id = imported(&result);
    assert_eq!(result["state"], "partial");
    let detail = details(&f.f.engine, &f.f.project.id, &id);
    assert_eq!(detail["coverage"]["structured_tests"], "unknown");
    assert!(
        f.f.engine
            .db
            .run(&id)
            .unwrap()
            .stages
            .iter()
            .all(|s| s.tests.is_none())
    );
    let list: ci::List = serde_json::from_value(json!({"project_id":f.f.project.id})).unwrap();
    assert_eq!(
        ci::runs(&f.f.engine, &list).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    rusqlite::Connection::open(f.f.engine.config.data_dir.join("preflight.sqlite3")).unwrap().execute("INSERT INTO management_trash(project_id,kind,target_id,deleted_at) VALUES (?1,'run',?2,?3)",rusqlite::params![f.f.project.id,id,1i64]).unwrap();
    assert!(
        ci::runs(&f.f.engine, &list).unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(imported(&f.sync(Some("100"), None, 2).await), id);
    assert!(
        ci::runs(&f.f.engine, &list).unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn schema_47_upgrade_preserves_native_run_and_logs_without_import_side_effects() {
    let f = common::Fixture::new();
    let run = f.start(&["echo", "native-m7-migration-marker"], 5);
    let run = common::finished(&f.engine, &run.id).await;
    assert_eq!(run.status, preflight::model::RunState::Succeeded);
    let logs = serde_json::to_value(f.engine.db.stage_logs(&run.id, None, 0).unwrap()).unwrap();
    let path = f.engine.config.data_dir.join("preflight.sqlite3");
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let c = rusqlite::Connection::open(&path).unwrap();
    common::remove_schema_53_for_historical_fixture(&c);
    c.execute_batch("DROP TABLE archive_encryption; DROP TABLE archive_disposals; DROP TABLE archive_gc_items; DROP TABLE archive_pin_requests; DROP TABLE archive_pins; DROP VIEW archive_inventory; DROP TABLE archive_imports;")
        .unwrap();
    for table in [
        "container_cleanup_requests",
        "container_jobs",
        "access_audit",
        "ci_sync_runs",
        "ci_test_pages",
        "ci_test_outputs",
        "ci_artifacts",
        "ci_log_parts",
        "ci_jobs",
        "ci_runs",
        "ci_source_state",
        "ci_syncs",
    ] {
        c.execute_batch(&format!("DROP TABLE {table}")).unwrap();
    }
    c.execute("DELETE FROM schema_version WHERE version>=48", [])
        .unwrap();
    c.execute("DELETE FROM migration_history WHERE version>=48", [])
        .unwrap();
    drop(c);
    let c = preflight::db::open(&path).unwrap();
    assert_eq!(
        c.query_row("SELECT max(version) FROM schema_version", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        preflight::migrations::VERSION as u64
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM ci_syncs", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    drop(c);
    let db = preflight::db::Db::new(&path).unwrap();
    assert_eq!(
        serde_json::to_value(db.run(&run.id).unwrap()).unwrap(),
        serde_json::to_value(run.clone()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(db.stage_logs(&run.id, None, 0).unwrap()).unwrap(),
        logs
    );
}
