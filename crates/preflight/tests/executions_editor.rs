#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{
    control, executions as x, experiments, model::*, pipeline_editor as ed, queues, schedules,
};
use serde_json::{Value, json};
fn definition(slow: bool) -> Value {
    json!({"version":1,"pipeline":{"stages":[{"id":"work","command":{"program":"sh","args":["-c",if slow{"sleep 30"}else{"printf completed"}],"timeout_seconds":40}}]}})
}
fn save(f: &common::Fixture, name: &str, revision: u64, slow: bool) {
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":name,"expected_revision":revision,"definition":definition(slow)})).unwrap()).unwrap();
}
fn query(project: Option<&str>, view: &str, kind: &str) -> x::Query {
    serde_json::from_value(json!({"project_id":project,"view":view,"kind":kind,"since":0})).unwrap()
}
fn cancel(
    project: &str,
    kind: &str,
    id: &str,
    ordinal: Option<usize>,
    revision: Option<u64>,
) -> x::Cancel {
    serde_json::from_value(json!({"project_id":project,"target_kind":kind,"target_id":id,"ordinal":ordinal,"revision":revision,"actor":"user","reason":"selected test cancellation"})).unwrap()
}
fn experiment(f: &common::Fixture, name: &str, base: &str) {
    experiments::save(&f.engine,&serde_json::from_value(json!({"project_id":f.project.id,"name":name,"expected_revision":0,"definition":{"version":1,"pipeline":{"name":base,"revision":1}}})).unwrap()).unwrap();
}
#[tokio::test]
async fn cross_project_parallel_work_and_independent_cancellation() {
    let f = common::Fixture::new();
    save(&f, "first", 0, true);
    experiment(&f, "trial", "first");
    let root = f.temp.path().join("second");
    std::fs::create_dir(&root).unwrap();
    let other = f
        .engine
        .register(RegisterProject {
            name: "second".into(),
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(json!({"project_id":other.id,"definition":definition(true)}))
                .unwrap(),
        )
        .unwrap();
    let exp=experiments::start(&f.engine,serde_json::from_value(json!({"project_id":f.project.id,"name":"trial","revision":1,"idempotency_key":"trial-run"})).unwrap()).unwrap();
    let exp_id = exp["run_id"].as_str().unwrap();
    for _ in 0..200 {
        if [&run.id, exp_id].iter().all(|id| {
            f.engine
                .db
                .run(id)
                .unwrap()
                .stages
                .iter()
                .any(|s| s.status == RunState::Running)
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!([&run.id, exp_id].iter().all(|id| {
        f.engine
            .db
            .run(id)
            .unwrap()
            .stages
            .iter()
            .any(|s| s.status == RunState::Running)
    }));
    let summary = x::summary(&f.engine, &x::Summary { since: now() }).unwrap();
    assert_eq!(summary["active_runs"], 2);
    assert_eq!(summary["current"]["total"], 2);
    assert_eq!(
        x::list(&f.engine, &query(None, "active", "experiment")).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        x::cancel(
            &f.engine,
            &cancel(&f.project.id, "run", &run.id, None, None)
        )
        .is_err()
    );
    x::cancel(&f.engine, &cancel(&other.id, "run", &run.id, None, None)).unwrap();
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    assert!(!f.engine.db.run(exp_id).unwrap().status.terminal());
    x::cancel(&f.engine, &cancel(&f.project.id, "run", exp_id, None, None)).unwrap();
    assert_eq!(
        common::finished(&f.engine, exp_id).await.status,
        RunState::Cancelled
    );
}
#[test]
fn revision_restore_preserves_history_and_reports_text_errors() {
    let f = common::Fixture::new();
    save(&f, "build", 0, false);
    save(&f, "build", 1, true);
    let result = ed::check(&ed::Check {
        text: "{\n \"version\": \"wrong\"\n}".into(),
    })
    .unwrap();
    assert_eq!(result["valid"], false);
    assert!(result["diagnostics"][0]["line"].as_u64().unwrap() > 1);
    assert_eq!(result["diagnostics"][0]["path"], "version");
    assert_eq!(
        ed::check(&ed::Check {
            text: "{\"version\":1,\"pipeline\":{\"stages\":[]}}".into()
        })
        .unwrap()["valid"],
        false
    );
    let restored = ed::restore(
        &f.engine,
        &ed::Restore {
            project_id: f.project.id.clone(),
            name: "build".into(),
            source_revision: 1,
            expected_revision: 2,
        },
    )
    .unwrap();
    assert_eq!(restored["revision"], 3);
    assert_eq!(restored["restored_from"], 1);
    let original = control::get(
        &f.project.root,
        &control::ConfigRef {
            project_id: f.project.id.clone(),
            name: "build".into(),
            revision: Some(1),
        },
    )
    .unwrap();
    assert_eq!(original["sha256"], restored["sha256"]);
    assert!(
        ed::restore(
            &f.engine,
            &ed::Restore {
                project_id: f.project.id.clone(),
                name: "build".into(),
                source_revision: 1,
                expected_revision: 2
            }
        )
        .is_err()
    );
    let page = ed::revisions(
        &f.engine,
        &ed::Revisions {
            project_id: f.project.id.clone(),
            name: "build".into(),
            before: None,
            limit: 2,
        },
    )
    .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["next_cursor"], 2);
    assert!(
        f.project
            .root
            .join(".preflight/pipelines/build/2.json")
            .exists()
    );
}
#[tokio::test]
async fn planned_experiment_dispatches_its_saved_revision_and_can_cancel_future_work() {
    let f = common::Fixture::new();
    save(&f, "base", 0, false);
    experiment(&f, "trial", "base");
    let request: schedules::Create=serde_json::from_value(json!({"project_id":f.project.id,"name":"trial-schedule","pipeline":{"name":"base","revision":1},"experiment":{"name":"trial","revision":1},"at_ms":now()+3600000,"grace_ms":60000,"misfire":"run_once","request_id":"trial-schedule","actor":"user","reason":"test future experiment"})).unwrap();
    let scheduled = schedules::create(&f.engine, &request).unwrap();
    let id = scheduled["schedule_id"].as_str().unwrap();
    assert_eq!(
        x::list(&f.engine, &query(None, "planned", "experiment")).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    x::cancel(
        &f.engine,
        &cancel(&f.project.id, "schedule", id, None, Some(1)),
    )
    .unwrap();
    assert_eq!(
        schedules::get(
            &f.engine,
            &schedules::Id {
                project_id: f.project.id.clone(),
                schedule_id: id.into()
            }
        )
        .unwrap()["state"],
        "cancelled"
    );
    let mut request = request;
    request.name = "due-trial".into();
    request.request_id = "due-trial".into();
    request.at_ms = now() - 1;
    let scheduled = schedules::create(&f.engine, &request).unwrap();
    schedules::run_due(
        &f.engine,
        &schedules::Due {
            project_id: Some(f.project.id.clone()),
            limit: 10,
        },
    )
    .unwrap();
    let result = schedules::get(
        &f.engine,
        &schedules::Id {
            project_id: f.project.id.clone(),
            schedule_id: scheduled["schedule_id"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    let run = result["run_id"].as_str().unwrap();
    assert_eq!(
        common::finished(&f.engine, run).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        x::list(&f.engine, &query(None, "recent", "experiment")).unwrap()["items"][0]["experiment_name"],
        "trial"
    );
}
#[tokio::test]
async fn pending_change_can_be_removed_without_cancelling_independent_siblings() {
    let f = common::Fixture::new();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&f.project.root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(f.project.root.join("input.txt"), "input").unwrap();
    for args in [vec!["add", "input.txt"], vec!["commit", "-m", "input"]] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&f.project.root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&f.project.root)
        .output()
        .unwrap();
    let commit = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    let q=queues::create(&f.engine,serde_json::from_value(json!({"project_id":f.project.id,"name":"work","request_id":"work","definition":definition(false),"mode":"independent","max_parallel":2,"changes":[{"id":"one","source":{"commit":commit,"patches":[]}},{"id":"two","source":{"commit":commit,"patches":[]}}]})).unwrap()).unwrap();
    let id = q["queue_id"].as_str().unwrap();
    x::cancel(
        &f.engine,
        &cancel(&f.project.id, "queue_item", id, Some(0), None),
    )
    .unwrap();
    let progress = x::summary(&f.engine, &x::Summary { since: 0 }).unwrap();
    assert_eq!(progress["queue"]["done"], 1);
    assert_eq!(progress["queue"]["total"], 2);
    queues::tick(&f.engine).unwrap();
    let status = queues::status(&f.engine, id, false).unwrap();
    assert_eq!(status["items"][0]["state"], "cancelled");
    let run = status["items"][1]["run_id"].as_str().unwrap();
    common::finished(&f.engine, run).await;
    queues::tick(&f.engine).unwrap();
    assert_eq!(
        queues::status(&f.engine, id, false).unwrap()["items"][1]["state"],
        "succeeded"
    );
    let gap=queues::create(&f.engine,serde_json::from_value(json!({"project_id":f.project.id,"name":"gap","request_id":"gap","definition":definition(true),"mode":"independent","changes":[{"id":"gap","source":{"commit":commit,"patches":[]}}]})).unwrap()).unwrap();
    let gap_id = gap["queue_id"].as_str().unwrap();
    queues::tick(&f.engine).unwrap();
    let status = queues::status(&f.engine, gap_id, false).unwrap();
    let run = status["items"][0]["run_id"].as_str().unwrap();
    let connection =
        rusqlite::Connection::open(f.temp.path().join("data/preflight.sqlite3")).unwrap();
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    connection
        .execute(
            "UPDATE change_queue_items SET run_id=NULL,state='pending' WHERE queue_id=?1",
            [gap_id],
        )
        .unwrap();
    x::cancel(
        &f.engine,
        &cancel(&f.project.id, "queue_item", gap_id, Some(0), None),
    )
    .unwrap();
    assert_eq!(
        common::finished(&f.engine, run).await.status,
        RunState::Cancelled
    );
    queues::tick(&f.engine).unwrap();
    assert_eq!(
        queues::status(&f.engine, gap_id, false).unwrap()["items"][0]["state"],
        "cancelled"
    );
}
#[tokio::test]
async fn new_tools_share_http_contracts_and_global_sse_reconnects_from_durable_state() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    save(&f, "base", 0, false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let engine = f.engine.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, preflight::api::router(engine))
            .await
            .unwrap();
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for (tool, path, args) in [
        ("executions_list", "executions", json!({"since":0})),
        (
            "executions_summary",
            "executions/summary",
            json!({"since":0}),
        ),
        (
            "pipeline_revisions",
            "pipeline-editor/revisions",
            json!({"project_id":f.project.id,"name":"base"}),
        ),
    ] {
        let result = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let actual: Value =
            serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
        let mut url = reqwest::Url::parse(&format!("http://{address}/api/v1/{path}")).unwrap();
        for (k, v) in args.as_object().unwrap() {
            url.query_pairs_mut().append_pair(
                k,
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
                    .as_str(),
            );
        }
        let expected: Value = http.get(url).send().await.unwrap().json().await.unwrap();
        assert_eq!(actual, expected);
    }
    let restored=client.call_tool(CallToolRequestParams::new("pipeline_restore").with_arguments(json!({"project_id":f.project.id,"name":"base","source_revision":1,"expected_revision":1}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(restored.is_error, Some(true));
    let restored: Value =
        serde_json::from_str(&restored.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(restored["revision"], 2);
    let args = json!({"text":"{\"version\":1,\"pipeline\":{\"stages\":[]}}"});
    let checked = client
        .call_tool(
            CallToolRequestParams::new("pipeline_check")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(checked.is_error, Some(true));
    let actual: Value = serde_json::from_str(&checked.content[0].as_text().unwrap().text).unwrap();
    let expected: Value = http
        .post(format!("http://{address}/api/v1/pipeline-editor/check"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual["valid"], false);
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(
                json!({"project_id":f.project.id,"definition":definition(true)}),
            )
            .unwrap(),
        )
        .unwrap();
    let result=client.call_tool(CallToolRequestParams::new("executions_cancel").with_arguments(json!({"project_id":f.project.id,"target_kind":"run","target_id":run.id,"actor":"agent","reason":"MCP cancellation acceptance"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    let large_args = json!({"text":"\n".repeat(65000)+"{}"});
    let large_check = client
        .call_tool(
            CallToolRequestParams::new("pipeline_check")
                .with_arguments(large_args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(large_check.is_error, Some(true));
    let large = http
        .post(format!("http://{address}/api/v1/pipeline-editor/check"))
        .json(&json!({"text":"\n".repeat(65000)+"{}"}))
        .send()
        .await
        .unwrap();
    assert_eq!(large.status(), 200);
    let result: Value = large.json().await.unwrap();
    assert_eq!(result["valid"], false);
    let mut stream = http
        .get(format!("http://{address}/api/v1/live?since=0"))
        .send()
        .await
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("\"global\""));
    drop(stream);
    client.cancel().await.unwrap();
    server.abort();
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
