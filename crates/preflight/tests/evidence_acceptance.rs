#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{acceptance, evidence, inspection, model::RunState};
use serde_json::{Value, json};
fn search(f: &common::Fixture, run: &str, query: &str) -> evidence::Search {
    serde_json::from_value(json!({"project_id":f.project.id,"run_id":run,"query":query})).unwrap()
}
fn policy() -> Value {
    json!([
     {"id":"build-ok","rule":{"kind":"stage_succeeded","stage_id":"build"}},
     {"id":"unit-cases","rule":{"kind":"tests","stage_id":"unit","min_passed":1,"max_failed":0,"require_complete":true}},
     {"id":"artifact","rule":{"kind":"artifact","stage_id":"build","name":"result","min_bytes":1}},
     {"id":"marker","rule":{"kind":"log_text","stage_id":"unit","text":"case-specific-marker","must_exist":true}},
     {"id":"duration","rule":{"kind":"duration","max_ms":60000}}
    ])
}
fn definition(criteria: Value) -> Value {
    json!({"version":1,"pipeline":{"acceptance":criteria,"stages":[
     {"id":"build","command":{"program":"sh","args":["-c","printf build-marker; printf result > result.bin"],"timeout_seconds":5},"outputs":{"result":{"path":"result.bin","type":"file"}}},
     {"id":"unit","kind":"test","needs":["build"],"command":{"program":"sh","args":["-c","cp template.json report.json"],"timeout_seconds":5},"testing":{"kind":"unit","reports":[{"format":"auto","path":"report.json"}]}}
    ]}})
}
#[tokio::test]
async fn adapter_cases_indexed_with_scoped_redacted_evidence_and_durable_acceptance() {
    let f = common::Fixture::new();
    std::fs::write(f.project.root.join("template.json"),json!({"version":1,"cases":[{"name":"same","suite":"unit","status":"passed","stdout":"case-specific-marker token=topsecret"},{"name":"same","suite":"unit","status":"skipped","stderr":"other-case-output"}]}).to_string()).unwrap();
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(
                json!({"project_id":f.project.id,"definition":definition(policy())}),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let verdict = acceptance::read(&f.engine, &run.id).unwrap();
    assert_eq!(verdict["status"], "accepted");
    assert_eq!(verdict["passed"], 5);
    let cases = inspection::tests(
        &f.engine,
        &inspection::Tests {
            project_id: f.project.id.clone(),
            run_id: run.id.clone(),
            offset: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(cases["total"], 2);
    assert_eq!(cases["stages"][1]["kind"], "unit");
    assert_ne!(
        cases["cases"][0]["case"]["id"],
        cases["cases"][1]["case"]["id"]
    );
    let mut request = search(&f, &run.id, "case-specific-marker");
    request.test_id = cases["cases"][0]["case"]["id"].as_str().map(str::to_owned);
    request.kind = Some("unit".into());
    let found = evidence::search(&f.engine, &request).unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 1);
    assert!(!found.to_string().contains("topsecret"));
    assert!(found.to_string().contains("REDACTED"));
    let read = evidence::Read {
        project_id: f.project.id.clone(),
        evidence_id: found["matches"][0]["evidence_id"].as_str().unwrap().into(),
        neighbors: 8,
        offset: None,
        max_bytes: None,
    };
    let expanded = evidence::read(&f.engine, &read).unwrap();
    assert!(!expanded.to_string().contains("other-case-output"));
    assert_eq!(expanded["test_id"], request.test_id.clone().unwrap());
    let context = evidence::context(&f.engine, &request).unwrap();
    assert!(!context["citations"].as_array().unwrap().is_empty());
    assert!(context["context"].as_str().unwrap().chars().count() <= 6000);
    f.engine
        .db
        .logs_archive(
            &serde_json::from_value(
                json!({"project_id":f.project.id,"run_id":run.id,"max_chunks":16}),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(evidence::read(&f.engine, &read).unwrap(), expanded);
    let foreign = common::Fixture::new();
    assert!(
        evidence::read(
            &f.engine,
            &evidence::Read {
                project_id: foreign.project.id.clone(),
                ..read
            }
        )
        .is_err()
    );
    std::fs::remove_file(f.project.root.join("template.json")).unwrap();
    assert_eq!(acceptance::read(&f.engine, &run.id).unwrap(), verdict);
}
#[tokio::test]
async fn missing_case_evidence_never_passes_and_rejected_policy_changes_run_outcome() {
    let f = common::Fixture::new();
    let d = json!({"version":1,"pipeline":{"acceptance":[{"id":"reported","rule":{"kind":"tests","stage_id":"test","min_passed":0,"max_failed":0,"require_complete":true}}],"stages":[{"id":"test","kind":"test","command":{"program":"sh","args":["-c","true"],"timeout_seconds":5},"testing":{"kind":"fuzz","reports":[]}}]}});
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(json!({"project_id":f.project.id,"definition":d})).unwrap(),
        )
        .unwrap();
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Failed
    );
    let report = acceptance::read(&f.engine, &run.id).unwrap();
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["criteria"][0]["status"], "unknown");
    let d = json!({"version":1,"pipeline":{"acceptance":[{"id":"marker","rule":{"kind":"log_text","text":"forbidden","must_exist":false}}],"stages":[{"id":"build","command":{"program":"sh","args":["-c","printf forbidden"],"timeout_seconds":5}}]}});
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(json!({"project_id":f.project.id,"definition":d})).unwrap(),
        )
        .unwrap();
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Failed
    );
    assert_eq!(
        acceptance::read(&f.engine, &run.id).unwrap()["status"],
        "rejected"
    );
}
#[test]
fn normalized_reports_validate_status_locations_and_do_not_invent_cases() {
    assert!(
        preflight::testing::parse_json(
            r#"{"version":1,"cases":[{"name":"bad","status":"success"}]}"#
        )
        .is_err()
    );
    assert!(
        preflight::testing::parse_json(
            r#"{"version":1,"cases":[{"name":"bad","status":"passed","file":"../secret"}]}"#
        )
        .is_err()
    );
    let report = preflight::testing::parse_json(r#"{"version":1,"cases":[]}"#).unwrap();
    assert!(report.cases_complete);
    assert_eq!(report.counts.passed, 0);
    let report=preflight::junit::parse(r#"<testsuite><testcase name="ok"><system-out>case &amp; marker</system-out><system-err><![CDATA[diagnostic]]></system-err></testcase></testsuite>"#).unwrap();
    let output = report.cases[0].output.as_ref().unwrap();
    assert_eq!(output.stdout, "case & marker");
    assert_eq!(output.stderr, "diagnostic");
}

#[tokio::test]
async fn mcp_and_http_retrieve_the_same_bounded_evidence_and_verdict() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    f.engine
        .db
        .append_stage_log(
            &run.id,
            "native",
            "stdout",
            100,
            "transport-evidence marker",
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
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
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for (tool, path, args) in [
        (
            "catalog_list",
            "catalog",
            json!({"kind":"pipelines","project_id":f.project.id,"limit":25}),
        ),
        (
            "evidence_search",
            "evidence/search",
            json!({"project_id":f.project.id,"run_id":run.id,"query":"transport-evidence"}),
        ),
        (
            "evidence_context",
            "evidence/context",
            json!({"project_id":f.project.id,"run_id":run.id,"query":"transport-evidence","max_chars":1024}),
        ),
        (
            "evidence_stats",
            "evidence/stats",
            json!({"project_id":f.project.id}),
        ),
        ("runs_acceptance", "", json!({"run_id":run.id})),
    ] {
        let result = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true), "{tool}");
        let actual: Value =
            serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
        let url = if tool == "runs_acceptance" {
            format!("http://{address}/api/v1/runs/{}/acceptance", run.id)
        } else {
            format!("http://{address}/api/v1/{path}")
        };
        let query = args
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                )
            })
            .collect::<Vec<_>>();
        let mut url = reqwest::Url::parse(&url).unwrap();
        url.query_pairs_mut().extend_pairs(query.iter());
        let expected: Value = http
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(actual, expected, "{tool}");
    }
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn report_ingestion_errors_are_retrievable_and_quiet_commands_have_execution_evidence() {
    let f = common::Fixture::new();
    std::fs::write(
        f.project.root.join("template.json"),
        r#"{"version":1,"cases":[{"name":"bad","status":"invented"}]}"#,
    )
    .unwrap();
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(
                json!({"project_id":f.project.id,"definition":definition(json!([]))}),
            )
            .unwrap(),
        )
        .unwrap();
    let completed = common::finished(&f.engine, &run.id).await;
    assert_eq!(completed.status, RunState::InfrastructureError);
    assert!(completed.stages[1].tests.is_none());
    let mut query = search(&f, &run.id, "case status");
    query.stage_id = Some("unit".into());
    let found = evidence::search(&f.engine, &query).unwrap();
    assert_eq!(found["matches"][0]["stream"], "diagnostic");
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    let mut query = search(&f, &run.id, "Preflight process started");
    query.stream = Some("lifecycle".into());
    assert!(
        !evidence::search(&f.engine, &query).unwrap()["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
