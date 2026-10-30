#[allow(dead_code)]
mod common;
use preflight::diagnostics::{self, Query};
use serde_json::{Value, json};
fn query(project: &str, run: &str, baseline: Option<&str>) -> Query {
    serde_json::from_value(json!({"project_id":project,"run_id":run,"baseline_id":baseline}))
        .unwrap()
}
fn append(f: &common::Fixture, run: &str, sequence: i64, text: &str) {
    f.engine
        .db
        .append_stage_log(run, "native", "stderr", sequence, text)
        .unwrap();
}
#[tokio::test]
async fn groups_compares_reconstructed_lines_and_survives_archival_restart() {
    let f = common::Fixture::new();
    let a = f.start(&[], 5);
    let b = f.start(&[], 5);
    common::finished(&f.engine, &a.id).await;
    common::finished(&f.engine, &b.id).await;
    append(
        &f,
        &a.id,
        100,
        &format!(
            "error[E0308]: mismatched types\n --> {}/stages/native/src/main.rs:3:2\nold.c:8:1: warning: unused variable\n",
            a.workspace.display()
        ),
    );
    append(&f, &b.id, 100, "error[E0308]: mismat");
    f.engine
        .db
        .append_stage_log(
            &b.id,
            "native",
            "stdout",
            100,
            "new.go:10:2: undefined: fresh\n",
        )
        .unwrap();
    append(
        &f,
        &b.id,
        101,
        &format!(
            "ched types\n --> {}/stages/native/src/main.rs:99:7\n",
            b.workspace.display()
        ),
    );
    append(
        &f,
        &b.id,
        102,
        "{\"level\":\"error\",\"message\":\"mismatched types\",\"code\":{\"code\":\"E0308\"},\"spans\":[{\"is_primary\":true,\"file_name\":\"src/main.rs\",\"line_start\":120,\"column_start\":4}]}\n",
    );
    append(&f, &b.id, 103, "ERROR: to");
    append(&f, &b.id, 104, "ken=private-diagnostic-secret invalid\n");
    let q = query(&f.project.id, &b.id, Some(&a.id));
    let report = diagnostics::report(&f.engine, &q).unwrap();
    assert_eq!(report["comparison_complete"], true);
    assert_eq!(
        report["group_counts"],
        json!({"fixed":1,"new":2,"recurring":1})
    );
    let groups = report["groups"].as_array().unwrap();
    let recurring = groups.iter().find(|g| g["state"] == "recurring").unwrap();
    assert_eq!(recurring["occurrences"], 2);
    assert_eq!(recurring["baseline_occurrences"], 1);
    assert_eq!(recurring["file"], "src/main.rs");
    assert_eq!(recurring["stage_id"], "native");
    assert!(!report.to_string().contains("private-diagnostic-secret"));
    assert!(report.to_string().contains("REDACTED"));
    let evidence=preflight::evidence::read(&f.engine,&serde_json::from_value(json!({"project_id":f.project.id,"evidence_id":recurring["evidence_ids"][0],"neighbors":2})).unwrap()).unwrap();
    assert_eq!(evidence["run_id"], b.id);
    for id in [&a.id, &b.id] {
        preflight::log_archive::archive(
            f.engine.clone(),
            preflight::log_archive::Request {
                project_id: f.project.id.clone(),
                run_id: id.clone(),
                max_chunks: 4,
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(diagnostics::report(&f.engine, &q).unwrap(), report);
    let config = f.engine.config.clone();
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(config).unwrap();
    assert_eq!(diagnostics::report(&reopened, &q).unwrap(), report);
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn partial_scans_never_claim_new_or_fixed_and_budgets_page_output() {
    let f = common::Fixture::new();
    let a = f.start(&[], 5);
    let b = f.start(&[], 5);
    common::finished(&f.engine, &a.id).await;
    common::finished(&f.engine, &b.id).await;
    append(&f, &a.id, 100, "gone.c:1:1: error: baseline-only\n");
    for n in 0..30 {
        append(
            &f,
            &b.id,
            100 + n,
            &format!("file.c:{n}:1: error: unique error number {n}\n"),
        );
    }
    let mut q = query(&f.project.id, &b.id, Some(&a.id));
    q.scan_events = 1;
    let partial = diagnostics::report(&f.engine, &q).unwrap();
    assert_eq!(partial["comparison_complete"], false);
    assert!(partial["group_counts"].get("fixed").is_none());
    assert!(partial["group_counts"].get("new").is_none());
    q.scan_events = 4096;
    q.limit = 100;
    q.max_bytes = 4096;
    let page = diagnostics::report(&f.engine, &q).unwrap();
    assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
    let offset = page["next_offset"].as_u64().unwrap();
    assert!(offset > 0);
    q.offset = offset as usize;
    let next = diagnostics::report(&f.engine, &q).unwrap();
    assert_ne!(
        page["groups"][0]["fingerprint"],
        next["groups"][0]["fingerprint"]
    );
    append(&f, &b.id, 500, &("x".repeat(65537) + "\n"));
    q.offset = 0;
    let overflow = diagnostics::report(&f.engine, &q).unwrap();
    assert_eq!(overflow["comparison_complete"], false);
    assert!(overflow["current_scan"]["omitted_lines"].as_u64().unwrap() > 0);
    assert!(overflow["group_counts"].get("fixed").is_none());
    let root = f.temp.path().join("foreign");
    std::fs::create_dir(&root).unwrap();
    let helper = if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    };
    std::fs::copy(f.project.root.join(helper), root.join(helper)).unwrap();
    let foreign = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "foreign".into(),
            path: root.to_string_lossy().into(),
        })
        .unwrap();
    let mut request = f.request(&[], 5);
    request.project_id = foreign.id.clone();
    let foreign_run = f.engine.start(request).unwrap();
    common::finished(&f.engine, &foreign_run.id).await;
    q.baseline_id = Some(foreign_run.id.clone());
    assert!(matches!(
        diagnostics::report(&f.engine, &q)
            .unwrap_err()
            .downcast_ref::<preflight::model::Fault>(),
        Some(preflight::model::Fault::NotFound)
    ));
    q.baseline_id = None;
    q.project_id = foreign.id;
    assert!(matches!(
        diagnostics::report(&f.engine, &q)
            .unwrap_err()
            .downcast_ref::<preflight::model::Fault>(),
        Some(preflight::model::Fault::NotFound)
    ));
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn mcp_and_http_return_same_bounded_report() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    append(&f, &run.id, 100, "main.c:4:2: error: parity diagnostic\n");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = preflight::api::router(f.engine.clone());
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
    let result = client
        .call_tool(
            CallToolRequestParams::new("diagnostics_report").with_arguments(
                json!({"project_id":f.project.id,"run_id":run.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let mcp: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    let rest: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "http://{addr}/api/v1/diagnostics?project_id={}&run_id={}",
            f.project.id, run.id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mcp, rest);
    assert_eq!(rest["groups"][0]["message"], "parity diagnostic");
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn incremental_large_event_cursor_survives_archival_and_restart_without_duplicate_counts() {
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    let text = "noise 🙂\n".repeat(500) + "error[E0308]: continued message\n --> src/last.rs:4:2\n";
    append(&f, &run.id, 100, &text);
    let mut q = query(&f.project.id, &run.id, None);
    q.scan_bytes = 1024;
    q.scan_events = 1;
    let first = diagnostics::report(&f.engine, &q).unwrap();
    assert_eq!(first["current_scan"]["complete"], false);
    preflight::log_archive::archive(
        f.engine.clone(),
        preflight::log_archive::Request {
            project_id: f.project.id.clone(),
            run_id: run.id.clone(),
            max_chunks: 4,
        },
    )
    .await
    .unwrap();
    let config = f.engine.config.clone();
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let e = preflight::engine::Engine::open(config).unwrap();
    let mut bytes = first["current_scan"]["bytes"].as_u64().unwrap();
    let mut report = first;
    for _ in 0..30 {
        report = diagnostics::report(&e, &q).unwrap();
        assert!(report["current_scan"]["bytes"].as_u64().unwrap() >= bytes);
        bytes = report["current_scan"]["bytes"].as_u64().unwrap();
        if report["current_scan"]["complete"] == true {
            break;
        }
    }
    assert_eq!(report["current_scan"]["complete"], true);
    assert_eq!(report["groups"].as_array().unwrap().len(), 1);
    assert_eq!(report["groups"][0]["occurrences"], 1);
    assert_eq!(report["groups"][0]["file"], "src/last.rs");
    assert_eq!(diagnostics::report(&e, &q).unwrap(), report);
    e.shutdown().await.unwrap();
}

#[tokio::test]
async fn byte_windows_preserve_full_unicode_event_and_strict_response_budget_after_archival() {
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    let text = "\0\"🙂汉字\n".repeat(2000);
    append(&f, &run.id, 100, &text);
    let event = f
        .engine
        .db
        .logs(&run.id, 0)
        .unwrap()
        .events
        .last()
        .unwrap()
        .id;
    let mut q:preflight::evidence::Read=serde_json::from_value(json!({"project_id":f.project.id,"evidence_id":format!("log:{event}"),"neighbors":0,"offset":0,"max_bytes":1024})).unwrap();
    let first = preflight::evidence::read(&f.engine, &q).unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= 1024);
    preflight::log_archive::archive(
        f.engine.clone(),
        preflight::log_archive::Request {
            project_id: f.project.id.clone(),
            run_id: run.id.clone(),
            max_chunks: 4,
        },
    )
    .await
    .unwrap();
    assert_eq!(preflight::evidence::read(&f.engine, &q).unwrap(), first);
    let mut result = String::new();
    for _ in 0..200 {
        let page = preflight::evidence::read(&f.engine, &q).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 1024);
        result.push_str(page["text"].as_str().unwrap());
        if let Some(next) = page["next_offset"].as_u64() {
            assert!(next > q.offset.unwrap() as u64);
            q.offset = Some(next as usize);
        } else {
            break;
        }
    }
    assert_eq!(result, text);
    q.offset = Some(3);
    assert!(preflight::evidence::read(&f.engine, &q).is_err());
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn background_index_finalizes_unterminated_live_line_after_own_run_cancel() {
    let f = common::Fixture::new();
    let run = f.start(&["sleep"], 10);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if f.engine
                .db
                .logs(&run.id, 0)
                .unwrap()
                .events
                .iter()
                .any(|e| e.text.contains("Preflight helper: stderr"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    append(&f, &run.id, 100, "ERROR: unterminated live message");
    let worker = tokio::spawn(preflight::diagnostics::run_indexer(f.engine.clone()));
    let database = f.engine.config.data_dir.join("preflight.sqlite3");
    tokio::time::timeout(std::time::Duration::from_secs(5),async{loop{let c=rusqlite::Connection::open(&database).unwrap();if c.query_row("SELECT count(*) FROM diagnostic_scans WHERE run_id=?1 AND drained=1 AND terminal_flushed=0",[&run.id],|r|r.get::<_,u64>(0)).unwrap()==1{break;}tokio::time::sleep(std::time::Duration::from_millis(20)).await;}}).await.unwrap();
    f.engine
        .cancel(preflight::model::CancelRun {
            run_id: run.id.clone(),
            actor: preflight::model::Actor::User,
            reason: "owned M5 acceptance cancellation".into(),
        })
        .unwrap();
    common::finished(&f.engine, &run.id).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let c = rusqlite::Connection::open(&database).unwrap();
            if c.query_row(
                "SELECT count(*) FROM diagnostic_scans WHERE run_id=?1 AND terminal_flushed=1",
                [&run.id],
                |r| r.get::<_, u64>(0),
            )
            .unwrap()
                == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let report = diagnostics::report(&f.engine, &query(&f.project.id, &run.id, None)).unwrap();
    assert_eq!(report["current_scan"]["complete"], true);
    assert_eq!(report["groups"][0]["message"], "unterminated live message");
    f.engine.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn full_m5_search_filters_use_actual_git_provenance_and_normalized_index() {
    let f = common::Fixture::new();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "M5 test"]);
    git(&["config", "user.email", "m5@example.invalid"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "fixture"]);
    let sha = git(&["rev-parse", "HEAD"]);
    let helper = if cfg!(windows) {
        "./helper.exe"
    } else {
        "./helper"
    };
    let run=f.engine.start_git_pipeline(serde_json::from_value(json!({"project_id":f.project.id,"source":{"commit":sha,"patches":[]},"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":helper,"args":[],"timeout_seconds":5}}]}}})).unwrap()).unwrap();
    common::finished(&f.engine, &run.id).await;
    f.engine
        .db
        .append_stage_log(
            &run.id,
            "build",
            "stderr",
            100,
            "src/main.c:3:2: error: lookup needle\nsrc/other.c:8:1: warning: other warning\n",
        )
        .unwrap();
    let mut q = query(&f.project.id, &run.id, None);
    q.severity = Some("error".into());
    q.file = Some("src/main.c".into());
    q.query = "lookup needle".into();
    let report = diagnostics::report(&f.engine, &q).unwrap();
    assert_eq!(report["groups"].as_array().unwrap().len(), 1);
    let event = report["groups"][0]["evidence_ids"][0]
        .as_str()
        .unwrap()
        .strip_prefix("log:")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let timestamp = f
        .engine
        .db
        .stage_logs(&run.id, Some("build"), 0)
        .unwrap()
        .events
        .into_iter()
        .find(|e| e.id == event)
        .unwrap()
        .timestamp;
    let mut filters = json!({"project_id":f.project.id,"batch_id":run.id,"query":"lookup needle","stage_id":"build","severity":"error","file":"src/main.c","git_sha":sha,"from_ms":timestamp,"to_ms":timestamp+1});
    let found =
        preflight::evidence::search(&f.engine, &serde_json::from_value(filters.clone()).unwrap())
            .unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 1);
    assert!(found["matches"][0]["ingested_at"].as_i64().unwrap() > 0);
    assert_eq!(found["diagnostic_filter_complete"], true);
    filters["git_sha"] = json!("f".repeat(40));
    assert!(
        preflight::evidence::search(&f.engine, &serde_json::from_value(filters.clone()).unwrap())
            .unwrap()["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    filters["git_sha"] = json!(sha);
    filters["from_ms"] = json!(timestamp + 1);
    filters["to_ms"] = json!(timestamp + 2);
    assert!(
        preflight::evidence::search(&f.engine, &serde_json::from_value(filters).unwrap()).unwrap()
            ["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(git(&["rev-parse", "HEAD"]), sha);
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_rustc_diagnostics_generic_run_comparison_and_mcp_resources_are_consistent() {
    use rmcp::{
        ServiceExt,
        model::{CallToolRequestParams, ReadResourceRequestParams},
        transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    std::fs::write(f.project.root.join("main.rs"), "fn main() {}\n").unwrap();
    let definition = json!({"version":1,"pipeline":{"stages":[{"id":"compile","command":{"program":"rustc","args":["--error-format=json","main.rs","-o","program"],"timeout_seconds":30},"outputs":{"program":{"path":"program","type":"executable"}}}]}});
    let start = || {
        f.engine
            .start_pipeline(
                serde_json::from_value(json!({"project_id":f.project.id,"definition":definition}))
                    .unwrap(),
            )
            .unwrap()
    };
    let baseline = start();
    assert_eq!(
        common::finished(&f.engine, &baseline.id).await.status,
        preflight::model::RunState::Succeeded
    );
    std::fs::write(
        f.project.root.join("main.rs"),
        "fn main(){let _value:i32=\"incorrect\";}\n",
    )
    .unwrap();
    let current = start();
    assert_eq!(
        common::finished(&f.engine, &current.id).await.status,
        preflight::model::RunState::Failed
    );
    let report = diagnostics::report(
        &f.engine,
        &query(&f.project.id, &current.id, Some(&baseline.id)),
    )
    .unwrap();
    assert_eq!(report["comparison_complete"], true);
    assert_eq!(report["groups"].as_array().unwrap().len(), 1);
    assert_eq!(report["groups"][0]["code"], "E0308");
    assert_eq!(report["groups"][0]["state"], "new");
    let compare:preflight::run_comparison::Query=serde_json::from_value(json!({"project_id":f.project.id,"run_id":current.id,"baseline_id":baseline.id,"max_bytes":4096})).unwrap();
    let result = preflight::run_comparison::compare(&f.engine, &compare).unwrap();
    assert!(serde_json::to_vec(&result).unwrap().len() <= 4096);
    assert_eq!(result["stage_changes"][0]["stage_id"], "compile");
    assert_eq!(result["artifact_changes_total"], 1);
    assert!(result["inputs"]["diff"]["tree_changed"].as_bool().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = preflight::api::router(f.engine.clone());
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
    let output=client.call_tool(CallToolRequestParams::new("runs_compare").with_arguments(json!({"project_id":f.project.id,"run_id":current.id,"baseline_id":baseline.id,"max_bytes":4096}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(output.is_error, Some(true));
    let mcp: Value = serde_json::from_str(&output.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(mcp, result);
    let rest:Value=reqwest::Client::builder().no_proxy().build().unwrap().get(format!("http://{addr}/api/v1/run-compare?project_id={}&run_id={}&baseline_id={}&max_bytes=4096",f.project.id,current.id,baseline.id)).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(rest, result);
    let resource = client
        .read_resource(ReadResourceRequestParams::new(format!(
            "preflight://runs/{}/diagnostics",
            current.id
        )))
        .await
        .unwrap();
    assert!(!resource.contents.is_empty());
    let run_resource = client
        .read_resource(ReadResourceRequestParams::new(format!(
            "preflight://runs/{}",
            current.id
        )))
        .await
        .unwrap();
    assert!(!run_resource.contents.is_empty());
    let digest = f.engine.db.artifacts(&baseline.id).unwrap()[0]
        .sha256
        .clone();
    let artifact = client
        .read_resource(ReadResourceRequestParams::new(format!(
            "preflight://projects/{}/artifacts/{digest}",
            f.project.id
        )))
        .await
        .unwrap();
    let serialized = serde_json::to_string(&artifact).unwrap();
    assert!(serialized.contains(&baseline.id));
    assert!(serialized.contains("metadata_only"));
    assert!(!serialized.contains("base64"));
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new(format!(
                "preflight://projects/{}/artifacts/{digest}",
                uuid::Uuid::now_v7()
            )))
            .await
            .is_err()
    );
    let templates = client.list_resource_templates(None).await.unwrap();
    assert!(
        templates
            .resource_templates
            .iter()
            .any(|t| t.uri_template.ends_with("/diagnostics"))
    );
    let compact = preflight::api::compact_report(&f.engine, &current.id).unwrap();
    assert_eq!(compact["diagnostics"]["indexed"], true);
    assert_eq!(compact["diagnostics"]["top"][0]["code"], "E0308");
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_missing_or_unexecuted_stage_never_proves_a_diagnostic_fixed() {
    let f = common::Fixture::new();
    let baseline = f.start(&[], 5);
    common::finished(&f.engine, &baseline.id).await;
    append(
        &f,
        &baseline.id,
        100,
        "old.c:3:2: error: must remain unknown\n",
    );
    let helper = if cfg!(windows) {
        "./helper.exe"
    } else {
        "./helper"
    };
    let current=f.engine.start_pipeline(serde_json::from_value(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"different-stage","command":{"program":helper,"args":[],"timeout_seconds":5}}]}}})).unwrap()).unwrap();
    common::finished(&f.engine, &current.id).await;
    let report = diagnostics::report(
        &f.engine,
        &query(&f.project.id, &current.id, Some(&baseline.id)),
    )
    .unwrap();
    assert_eq!(report["current_scan"]["complete"], true);
    assert_eq!(report["baseline_scan"]["complete"], true);
    assert_eq!(report["comparison_complete"], false);
    assert!(report["group_counts"].get("fixed").is_none());
    assert_eq!(
        report["groups"][0]["state"],
        "not_observed_in_current_window"
    );
    f.engine.shutdown().await.unwrap();
}
