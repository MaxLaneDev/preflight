mod common;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use common::*;
use preflight::{api::router, model::*};
use rmcp::{
    ServiceExt as McpServiceExt, model::CallToolRequestParams,
    transport::StreamableHttpClientTransport,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn request(f: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router(f.engine.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Host", "localhost")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn rest_register_start_poll_logs_cancel_and_boundaries() {
    let f = Fixture::new();
    assert!(f.temp.path().exists());
    let (status, project) = request(
        &f,
        "POST",
        "/api/v1/projects",
        json!({"name":"repeat","path":f.project.root}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project["id"], f.project.id);
    let (status, run) = request(
        &f,
        "POST",
        "/api/v1/runs",
        serde_json::to_value(f.request(&[], 5)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = run["id"].as_str().unwrap();
    assert_eq!(finished(&f.engine, id).await.status, RunState::Succeeded);
    let (_, report) = request(&f, "GET", &format!("/api/v1/runs/{id}/report"), Value::Null).await;
    assert_eq!(report["status"], "succeeded");
    assert!(report["tests"]["passed"].is_null());
    assert!(output(&f.engine, id, "stdout").contains("stdout"));
    let (_, logs) = request(&f, "GET", &format!("/api/v1/runs/{id}/logs"), Value::Null).await;
    assert!(!logs["events"].as_array().unwrap().is_empty());
    let run = f.start(&["sleep"], 10);
    let (status, _) = request(
        &f,
        "POST",
        &format!("/api/v1/runs/{}/cancel", run.id),
        json!({"actor":"user","reason":"REST test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    let (status, error) = request(&f, "GET", "/api/v1/runs/missing", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "not_found");
    for (host, origin) in [
        ("evil.example", "http://evil.example"),
        ("localhost", "http://evil.example"),
    ] {
        let response = router(f.engine.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/runs")
                    .header("Host", host)
                    .header("Origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let oversized = router(f.engine.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/runs")
                .header("Host", "localhost")
                .header("Content-Type", "application/json")
                .body(Body::from(vec![b' '; 65537]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn mcp_http_sdk_handshake_and_vertical_slice() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(f.engine.clone());
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let transport = StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp"));
    let client = ().serve(transport).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "build_start"));
    let registered = client
        .call_tool(
            CallToolRequestParams::new("projects_register").with_arguments(
                json!({"name":"MCP project","path":f.project.root})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(registered.is_error, Some(true));
    let started = client
        .call_tool(
            CallToolRequestParams::new("build_start").with_arguments(
                serde_json::to_value(f.request(&[], 5))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(started.is_error, Some(true));
    let value: Value = serde_json::from_str(&started.content[0].as_text().unwrap().text).unwrap();
    let id = value["run_id"].as_str().unwrap();
    assert_eq!(finished(&f.engine, id).await.status, RunState::Succeeded);
    let configured = client
        .call_tool(
            CallToolRequestParams::new("runs_definition")
                .with_arguments(json!({"run_id":id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(configured.is_error, Some(true));
    let configured: Value =
        serde_json::from_str(&configured.content[0].as_text().unwrap().text).unwrap();
    assert!(configured["definition"].is_null());
    assert!(configured["native_command"].is_object());
    let (code, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/runs/{id}/definition"),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(rest, configured);
    for tool in ["build_status", "build_report", "logs_read"] {
        let response = client
            .call_tool(
                CallToolRequestParams::new(tool)
                    .with_arguments(json!({"run_id":id}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true));
        let result: Value =
            serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
        if tool == "logs_read" {
            assert!(!result["events"].as_array().unwrap().is_empty())
        } else {
            assert_eq!(result["status"], "succeeded")
        }
    }
    let slow = f.start(&["sleep"], 10);
    let response = client
        .call_tool(
            CallToolRequestParams::new("build_cancel").with_arguments(
                json!({"run_id":slow.id,"actor":"agent","reason":"MCP cancellation"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    assert_eq!(
        finished(&f.engine, &slow.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.run(&slow.id).unwrap().cancel_actor.as_deref(),
        Some("agent")
    );
    let bad = client
        .call_tool(
            CallToolRequestParams::new("build_status")
                .with_arguments(json!({"run_id":"missing"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(bad.is_error, Some(true));
    let mut config = Fixture::config(f.temp.path());
    config.bind = address;
    let config_path = f.temp.path().join("cli.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
    let cli = async |args: Vec<String>| {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
            .arg("--config")
            .arg(&config_path)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let run = cli(vec![
        "build".into(),
        f.project.root.to_string_lossy().into_owned(),
        "--".into(),
        f.request(&[], 5).command.program,
        "sleep".into(),
    ])
    .await;
    let cli_id = run["id"].as_str().unwrap();
    let status = cli(vec!["status".into(), cli_id.into()]).await;
    assert_eq!(status["run_id"], cli_id);
    cli(vec![
        "cancel".into(),
        cli_id.into(),
        "CLI cancellation".into(),
    ])
    .await;
    let done = finished(&f.engine, cli_id).await;
    assert_eq!(done.status, RunState::Cancelled);
    assert_eq!(done.cancel_actor.as_deref(), Some("user"));
    let definition = json!({"version":1,"pipeline":{"stages":[
        {"id":"compile","command":{"program":f.request(&[],5).command.program,"args":[]}},
        {"id":"existing-tests","kind":"test","needs":["compile"],"command":{"program":f.request(&[],5).command.program,"args":["fail"]}}
    ]}});
    let validation = client
        .call_tool(
            CallToolRequestParams::new("pipeline_validate")
                .with_arguments(definition.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(validation.is_error, Some(true));
    let pipeline = client
        .call_tool(
            CallToolRequestParams::new("pipeline_start").with_arguments(
                json!({"project_id":f.project.id,"definition":definition})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(pipeline.is_error, Some(true));
    let started: Value =
        serde_json::from_str(&pipeline.content[0].as_text().unwrap().text).unwrap();
    let pipeline_id = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, pipeline_id).await.status,
        RunState::Failed
    );
    let result = client
        .call_tool(
            CallToolRequestParams::new("build_report")
                .with_arguments(json!({"run_id":pipeline_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let report: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(report["outcome"], "tests_failed");
    assert_eq!(report["build"]["status"], "succeeded");
    assert!(report["tests"]["passed"].is_null());
    for tool in ["stages_get", "artifacts_list"] {
        let result = client
            .call_tool(
                CallToolRequestParams::new(tool)
                    .with_arguments(json!({"run_id":pipeline_id}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }
    std::fs::write(
        f.project.root.join("preflight.yaml"),
        serde_json::to_string(&definition).unwrap(),
    )
    .unwrap();
    for command in ["build", "test"] {
        let run = cli(vec![
            command.into(),
            f.project.root.to_string_lossy().into_owned(),
        ])
        .await;
        let id = run["id"].as_str().unwrap();
        assert_eq!(finished(&f.engine, id).await.status, RunState::Failed);
    }
    let (status, _) = request(&f, "POST", "/api/v1/pipelines/validate", definition.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, run) = request(
        &f,
        "POST",
        "/api/v1/pipeline-runs",
        json!({"project_id":f.project.id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rest_id = run["id"].as_str().unwrap();
    assert_eq!(finished(&f.engine, rest_id).await.status, RunState::Failed);
    let (_, logs) = request(
        &f,
        "GET",
        &format!("/api/v1/runs/{rest_id}/logs?stage=existing-tests"),
        Value::Null,
    )
    .await;
    assert!(
        logs["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["stage_id"] == "existing-tests")
    );

    let suite = json!({"version":1,"suite":"sdk","adapter":"helper","cases":[{"id":"compile-only","files":{"main.my":"source"},"execute":{"mode":"compile","entry":"main.my"},"expect":{"compile":{"exit_code":0,"stdout":{"contains":"stdout"}}}}]});
    let adapter = json!({"name":"helper","source_extension":"my","compile":{"program":"{compiler}","args":[]},"timeout_seconds":5,"capabilities":["compile"]});
    let build = json!({"version":1,"pipeline":{"stages":[{"id":"copy","command":{"program":f.request(&[],5).command.program,"args":[]},"outputs":{}}]}});
    // Publish a copied helper via rustc so the fixture compiler is a real immutable artifact.
    std::fs::write(
        f.project.root.join("compiler.rs"),
        "fn main(){println!(\"stdout from fixture compiler\");}",
    )
    .unwrap();
    let mut build = build;
    build["pipeline"]["stages"][0]["command"] =
        json!({"program":"rustc","args":["compiler.rs","-o","compiler.exe"]});
    build["pipeline"]["stages"][0]["outputs"] =
        json!({"compiler":{"path":"compiler.exe","type":"executable"}});
    let batch = json!({"version":1,"build":build,"compiler":"${artifact.copy.compiler}","adapter":adapter,"suites":["suite.yaml"]});
    std::fs::write(
        f.project.root.join("suite.yaml"),
        serde_json::to_vec(&suite).unwrap(),
    )
    .unwrap();
    std::fs::write(
        f.project.root.join("preflight-fixtures.yaml"),
        serde_json::to_vec(&batch).unwrap(),
    )
    .unwrap();
    for (tool, args) in [
        (
            "fixtures_validate",
            json!({"adapter":adapter,"suite":suite}),
        ),
        ("fixtures_list", json!({"project_id":f.project.id})),
    ] {
        let r = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true), "{r:?}");
    }
    let r = client
        .call_tool(
            CallToolRequestParams::new("batch_start").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true), "{r:?}");
    let r: Value = serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap();
    let batch_id = r["batch_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, batch_id).await.status,
        RunState::Succeeded
    );
    for tool in ["batch_status", "batch_report", "fixtures_results"] {
        let r = client
            .call_tool(
                CallToolRequestParams::new(tool)
                    .with_arguments(json!({"run_id":batch_id}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true));
    }
    let r = client
        .call_tool(
            CallToolRequestParams::new("fixtures_diff").with_arguments(
                json!({"run_id":batch_id,"stage_id":"fixtures-sdk","case_id":"compile-only"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let (_, r) = request(
        &f,
        "GET",
        &format!("/api/v1/runs/{batch_id}/fixtures/fixtures-sdk/compile-only"),
        Value::Null,
    )
    .await;
    assert_eq!(r["status"], "succeeded");
    let (_, r) = request(
        &f,
        "GET",
        &format!("/api/v1/batches/{batch_id}/report"),
        Value::Null,
    )
    .await;
    assert_eq!(r["fixtures"]["counts"]["succeeded"], 1);
    let run = cli(vec![
        "fixtures".into(),
        "run".into(),
        f.project.root.to_string_lossy().into_owned(),
    ])
    .await;
    assert_eq!(
        finished(&f.engine, run["id"].as_str().unwrap())
            .await
            .status,
        RunState::Succeeded
    );

    let publication = json!({"run_id":run["id"],"stage_id":"copy","artifact_name":"compiler"});
    let published = client
        .call_tool(
            CallToolRequestParams::new("modules_publish")
                .with_arguments(publication.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(published.is_error, Some(true), "{published:?}");
    let module: Value =
        serde_json::from_str(&published.content[0].as_text().unwrap().text).unwrap();
    let digest = module["sha256"].as_str().unwrap();
    let (status, repeated) = request(&f, "POST", "/api/v1/modules", publication).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated, module);
    let (status, fetched) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/modules?project_id={}&sha256={digest}",
            f.project.id
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, module);
    let result = client
        .call_tool(
            CallToolRequestParams::new("modules_get").with_arguments(
                json!({"project_id":f.project.id,"sha256":digest})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let result=client.call_tool(CallToolRequestParams::new("pipeline_start").with_arguments(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"module-test","command":{"program":format!("${{module.{digest}}}"),"args":[]}}]}}}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    let started: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    let consumer = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, consumer).await.status,
        RunState::Succeeded
    );
    let result = client
        .call_tool(
            CallToolRequestParams::new("runs_module_inputs")
                .with_arguments(json!({"run_id":consumer}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let recorded: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(recorded["entries"][0]["producer"]["sha256"], digest);
    let (status, paths) = request(
        &f,
        "GET",
        &format!("/api/v1/run-module-inputs?run_id={consumer}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paths, recorded);

    for (tool, args) in [
        ("runs_inputs", json!({"run_id":run["id"]})),
        (
            "runs_compare_inputs",
            json!({"run_id":run["id"],"baseline_id":run["id"]}),
        ),
    ] {
        let response = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true));
    }

    std::fs::write(f.project.root.join("tree-producer.rs"),r#"fn main(){if let Some(path)=std::env::args().nth(1){assert_eq!(std::fs::read_to_string(std::path::Path::new(&path).join("lib/module")).unwrap(),"tree payload");println!("tree consumed");}else{std::fs::create_dir_all("sdk/lib").unwrap();std::fs::create_dir("sdk/empty").unwrap();std::fs::write("sdk/lib/module","tree payload").unwrap();}}"#).unwrap();
    let result=client.call_tool(CallToolRequestParams::new("pipeline_start").with_arguments(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[
        {"id":"compile-tree","command":{"program":"rustc","args":["tree-producer.rs","-o","tree-runner.exe"]},"outputs":{"runner":{"path":"tree-runner.exe","type":"executable"}}},
        {"id":"produce-tree","needs":["compile-tree"],"command":{"program":"${artifact.compile-tree.runner}","args":[]},"outputs":{"sdk":{"path":"sdk","type":"directory"}}}
    ]}}}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let producer: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    let producer_id = producer["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, producer_id).await.status,
        RunState::Succeeded
    );
    let (status, tree) = request(
        &f,
        "POST",
        "/api/v1/modules",
        json!({"run_id":producer_id,"stage_id":"produce-tree","artifact_name":"sdk"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tree["kind"], "directory");
    let (status, runner) = request(
        &f,
        "POST",
        "/api/v1/modules",
        json!({"run_id":producer_id,"stage_id":"compile-tree","artifact_name":"runner"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tree_hash = tree["sha256"].as_str().unwrap();
    let runner_hash = runner["sha256"].as_str().unwrap();
    let result = client
        .call_tool(
            CallToolRequestParams::new("modules_manifest").with_arguments(
                json!({"project_id":f.project.id,"sha256":tree_hash})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let manifest: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(manifest["total"], 3);
    let (status, rest_manifest) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/module-manifest?project_id={}&sha256={tree_hash}",
            f.project.id
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(manifest, rest_manifest);
    let result=client.call_tool(CallToolRequestParams::new("pipeline_start").with_arguments(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"tree-consumer","command":{"program":format!("${{module.{runner_hash}}}"),"args":[format!("${{module.{tree_hash}}}")]}}]}}}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let consumer: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    let consumer_id = consumer["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, consumer_id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, consumer_id, "stdout").contains("tree consumed"));
    let (_, paths) = request(
        &f,
        "GET",
        &format!("/api/v1/run-module-inputs?run_id={consumer_id}"),
        Value::Null,
    )
    .await;
    assert!(paths["entries"].as_array().unwrap().iter().any(|e|e["producer"]["kind"]=="directory" && e["producer"]["run_id"]==producer_id));
    for tool in ["server_capabilities", "pipeline_schema", "scheduler_status"] {
        let r = client
            .call_tool(CallToolRequestParams::new(tool))
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true));
    }
    let (status, scheduler) = request(&f, "GET", "/api/v1/scheduler", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(scheduler["capacity_slots"], 2);
    assert_eq!(scheduler["default_stage_slots"], 1);
    let saved = json!({"project_id":f.project.id,"name":"sdk-saved","expected_revision":0,"definition":definition});
    let r = client
        .call_tool(
            CallToolRequestParams::new("pipeline_save")
                .with_arguments(saved.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let (status, _) = request(&f, "POST", "/api/v1/pipeline-configs", saved).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let r = client
        .call_tool(
            CallToolRequestParams::new("pipeline_run_saved").with_arguments(
                json!({"project_id":f.project.id,"name":"sdk-saved","revision":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let r: Value = serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(
        finished(&f.engine, r["run_id"].as_str().unwrap())
            .await
            .status,
        RunState::Failed
    );
    for (tool, args) in [
        (
            "pipeline_get",
            json!({"project_id":f.project.id,"name":"sdk-saved"}),
        ),
        ("pipeline_list", json!({"project_id":f.project.id})),
        ("projects_get", json!({"project_id":f.project.id})),
        (
            "runs_list",
            json!({"project_id":f.project.id,"status":"failed","limit":2}),
        ),
    ] {
        let r = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true));
    }

    let profile = json!({"project_id":f.project.id,"name":"generic-tools","expected_revision":0,"definition":{"version":1,"tools":{"runner":{"program":f.request(&[],5).command.program,"probe_args":[]}},"env":{"PROFILE_SOURCE":"mcp"}}});
    let r = client
        .call_tool(
            CallToolRequestParams::new("toolchain_save")
                .with_arguments(profile.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    for (tool, args) in [
        ("toolchain_schema", json!({})),
        (
            "toolchain_get",
            json!({"project_id":f.project.id,"name":"generic-tools","revision":1}),
        ),
        ("toolchain_list", json!({"project_id":f.project.id})),
    ] {
        let r = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true));
    }
    let probe = json!({"project_id":f.project.id,"name":"generic-tools","revision":1,"idempotency_key":"sdk-probe"});
    let r = client
        .call_tool(
            CallToolRequestParams::new("toolchain_probe")
                .with_arguments(probe.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let r: Value = serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap();
    let (_, duplicate) = request(&f, "POST", "/api/v1/toolchain-probes", probe).await;
    assert_eq!(duplicate["id"], r["run_id"]);
    assert_eq!(
        finished(&f.engine, r["run_id"].as_str().unwrap())
            .await
            .status,
        RunState::Succeeded
    );
    let submission = json!({"project_id":f.project.id,"name":"sdk-saved","revision":1,"profile":{"name":"generic-tools","revision":1},"idempotency_key":"sdk-saved-start"});
    let r = client
        .call_tool(
            CallToolRequestParams::new("pipeline_run_saved")
                .with_arguments(submission.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let r: Value = serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap();
    let (_, same) = request(
        &f,
        "POST",
        "/api/v1/saved-pipeline-runs",
        submission.clone(),
    )
    .await;
    assert_eq!(same["id"], r["run_id"]);
    let mut changed = submission;
    changed["profile"] = Value::Null;
    let (status, conflict) = request(&f, "POST", "/api/v1/saved-pipeline-runs", changed).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "idempotency_conflict");
    let run = finished(&f.engine, r["run_id"].as_str().unwrap()).await;
    assert_eq!(run.stages[0].definition.env["PROFILE_SOURCE"], "mcp");

    std::fs::write(
        f.project.root.join("git-proof.rs"),
        "fn main(){println!(\"commit\");}\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Preflight SDK test"]);
    git(&["config", "user.email", "sdk@example.invalid"]);
    git(&["add", "git-proof.rs"]);
    git(&["commit", "-qm", "SDK source"]);
    let commit = git(&["rev-parse", "HEAD"]).trim().to_owned();
    let patch = "diff --git a/git-proof.rs b/git-proof.rs\n--- a/git-proof.rs\n+++ b/git-proof.rs\n@@ -1 +1 @@\n-fn main(){println!(\"commit\");}\n+fn main(){println!(\"patched\");}\n";
    let git_request = json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"rustc","args":["git-proof.rs","-o","git-proof.exe"]},"outputs":{"binary":{"path":"git-proof.exe","type":"executable"}}},{"id":"run","needs":["build"],"command":{"program":"${artifact.build.binary}","args":[]}}]}},"source":{"commit":commit,"patches":[{"name":"change-output","diff":patch}]}});
    let response = client
        .call_tool(
            CallToolRequestParams::new("pipeline_start_git")
                .with_arguments(git_request.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let source_id = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, source_id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, source_id, "stdout").contains("patched"));
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_source")
                .with_arguments(json!({"run_id":source_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let evidence: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(evidence["prepared"]["resolved_commit"], commit);
    let (status, rest_evidence) = request(
        &f,
        "GET",
        &format!("/api/v1/run-source?run_id={source_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest_evidence, evidence);
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_source_patch").with_arguments(
                json!({"run_id":source_id,"ordinal":0})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let stored_patch: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(stored_patch["patch"]["diff"], patch);
    let (status, rest_patch) = request(
        &f,
        "GET",
        &format!("/api/v1/run-source-patch?run_id={source_id}&ordinal=0"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest_patch, stored_patch);
    let (status, rest_run) = request(&f, "POST", "/api/v1/git-pipeline-runs", git_request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rest_id = rest_run["id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, rest_id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine.db.source_evidence(rest_id).unwrap()["tree_sha256"],
        evidence["tree_sha256"]
    );
    assert!(
        std::fs::read_to_string(f.project.root.join("git-proof.rs"))
            .unwrap()
            .contains("commit")
    );
    std::fs::write(
        f.project.root.join("git-proof.rs"),
        "fn main(){println!(\"dirty transport\");}\n",
    )
    .unwrap();
    let dirty_request = json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"rustc","args":["git-proof.rs","-o","git-proof.exe"]},"outputs":{"binary":{"path":"git-proof.exe","type":"executable"}}},{"id":"run","needs":["build"],"command":{"program":"${artifact.build.binary}","args":[]}}]}},"source":{"expected_head":commit,"untracked":[]}});
    let response = client
        .call_tool(
            CallToolRequestParams::new("pipeline_start_dirty")
                .with_arguments(dirty_request.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let dirty_id = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, dirty_id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, dirty_id, "stdout").contains("dirty transport"));
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_source")
                .with_arguments(json!({"run_id":dirty_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let evidence: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(evidence["mode"], "git_dirty");
    let (status, rest_evidence) = request(
        &f,
        "GET",
        &format!("/api/v1/run-source?run_id={dirty_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest_evidence, evidence);
    let (status, rest_run) =
        request(&f, "POST", "/api/v1/dirty-pipeline-runs", dirty_request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rest_id = rest_run["id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, rest_id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine.db.source_evidence(rest_id).unwrap()["tree_sha256"],
        evidence["tree_sha256"]
    );
    let multi_request = json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"rustc","args":["app/git-proof.rs","-o","program.exe"]},"outputs":{"binary":{"path":"program.exe","type":"executable"}}},{"id":"run","needs":["build"],"command":{"program":"${artifact.build.binary}","args":[]}}]}},"source":{"roots":[{"name":"app","project_id":f.project.id,"path":"app","revision":{"mode":"git","source":{"commit":commit,"patches":[{"name":"change-output","diff":patch}]}}},{"name":"other","project_id":f.project.id,"path":"other","revision":{"mode":"dirty","source":{}}}]}});
    let response = client
        .call_tool(
            CallToolRequestParams::new("pipeline_start_multi")
                .with_arguments(multi_request.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let multi_id = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, multi_id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, multi_id, "stdout").contains("patched"));
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_source_root_inputs").with_arguments(
                json!({"run_id":multi_id,"root_name":"other"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let page: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (status, rest_page) = request(
        &f,
        "GET",
        &format!("/api/v1/run-source-root-inputs?run_id={multi_id}&root_name=other"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest_page, page);
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_source_patch").with_arguments(
                json!({"run_id":multi_id,"root_name":"app","ordinal":0})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let stored: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(stored["patch"]["diff"], patch);
    let (status, rest_patch) = request(
        &f,
        "GET",
        &format!("/api/v1/run-source-patch?run_id={multi_id}&root_name=app&ordinal=0"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest_patch, stored);
    let (status, rest_run) =
        request(&f, "POST", "/api/v1/multi-pipeline-runs", multi_request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rest_id = rest_run["id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, rest_id).await.status,
        RunState::Succeeded
    );
    let mcp_evidence = f.engine.db.source_evidence(multi_id).unwrap();
    let rest_evidence = f.engine.db.source_evidence(rest_id).unwrap();
    assert_eq!(
        mcp_evidence["prepared"]["aggregate_sha256"],
        rest_evidence["prepared"]["aggregate_sha256"]
    );
    assert_eq!(mcp_evidence["tree_sha256"], rest_evidence["tree_sha256"]);
    std::fs::create_dir(f.project.root.join("pinned-sdk")).unwrap();
    std::fs::write(f.project.root.join("pinned-sdk/version"), "sdk transport").unwrap();
    let response=client.call_tool(CallToolRequestParams::new("toolchain_inspect_pin").with_arguments(json!({"project_id":f.project.id,"path":f.request(&[],5).command.program,"kind":"executable"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let tool: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (status, sdk) = request(
        &f,
        "POST",
        "/api/v1/toolchain-inspect-pin",
        json!({"project_id":f.project.id,"path":"pinned-sdk","kind":"directory"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let profile = json!({"project_id":f.project.id,"name":"pinned","expected_revision":0,"definition":{"version":1,"tools":{"runner":{"program":"runner","probe_args":[]}},"pins":{"tools":{"runner":{"path":tool["path"],"sha256":tool["sha256"]}},"sdks":{"platform":{"path":"pinned-sdk","sha256":sdk["sha256"]}},"target":"transport-test","abi":{"format":"1"}}}});
    let response = client
        .call_tool(
            CallToolRequestParams::new("toolchain_save")
                .with_arguments(profile.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let pinned = json!({"project_id":f.project.id,"profile":{"name":"pinned","revision":1},"idempotency_key":"pinned-test","definition":{"version":1,"pipeline":{"stages":[{"id":"write","command":{"program":"${tool.runner}","args":["write"]},"outputs":{"file":{"path":"result.txt","type":"file"}}}]}}});
    let response = client
        .call_tool(
            CallToolRequestParams::new("pipeline_start_pinned")
                .with_arguments(pinned.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let pinned_id = started["run_id"].as_str().unwrap();
    assert_eq!(
        finished(&f.engine, pinned_id).await.status,
        RunState::Succeeded
    );
    let (status, duplicate) = request(&f, "POST", "/api/v1/pinned-pipeline-runs", pinned).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(duplicate["id"], pinned_id);
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_toolchain")
                .with_arguments(json!({"run_id":pinned_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let evidence: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (status, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/run-toolchain?run_id={pinned_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest, evidence);
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_stage_key").with_arguments(
                json!({"run_id":pinned_id,"stage_id":"write"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let key: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(key["evidence"]["reusable"], false);
    let (status, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/stage-key?run_id={pinned_id}&stage_id=write"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key, rest);
    for _ in 0..100 {
        if f.engine.db.operations(pinned_id).unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["state"] == "released")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_operations")
                .with_arguments(json!({"run_id":pinned_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let operations: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(operations["total"], 2);
    assert!(
        operations["operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["state"] == "released")
    );
    let (status, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/run-operations?run_id={pinned_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(operations, rest);
    let response = client
        .call_tool(
            CallToolRequestParams::new("runs_cache")
                .with_arguments(json!({"run_id":pinned_id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let cache: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(cache["stages"][0]["decision"]["status"], "disabled");
    let (status, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/run-cache?run_id={pinned_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest, cache);
    let disabled_bridge = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_bind").with_arguments(
                json!({"run_id":pinned_id,"thread_id":"01a17f49-a837-7553-a343-bf597124dae1"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(disabled_bridge.is_error, Some(true));
    assert_eq!(
        f.engine.db.codex_bindings(pinned_id).unwrap()["bindings"],
        json!([])
    );

    let response = client
        .call_tool(
            CallToolRequestParams::new("cache_cleanup_preview").with_arguments(
                json!({"project_id":f.project.id,"pipeline_id":"compiler"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let plan: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(plan["physical_bytes_to_delete"], 0);
    let response = client
        .call_tool(
            CallToolRequestParams::new("cache_cleanup_execute").with_arguments(
                json!({"plan_id":plan["plan_id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let executed: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (status, repeated) = request(
        &f,
        "POST",
        "/api/v1/cache-cleanup-execute",
        json!({"plan_id":plan["plan_id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(executed, repeated);

    let (status, module) = request(
        &f,
        "POST",
        "/api/v1/modules",
        json!({"run_id":pinned_id,"stage_id":"write","artifact_name":"file"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let response = client
        .call_tool(
            CallToolRequestParams::new("modules_contracts").with_arguments(
                json!({"project_id":f.project.id,"sha256":module["sha256"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let contracts: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (status, rest) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/module-contracts?project_id={}&sha256={}",
            f.project.id,
            module["sha256"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest, contracts);
    assert!(
        contracts["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["contract_sha256"] == evidence["verified"]["contract_sha256"])
    );
    let selected_pipeline=f.engine.start_pipeline(preflight::pipeline::StartPipeline {project_id:f.project.id.clone(),definition:Some(serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"first","command":{"program":f.request(&[],5).command.program,"args":["sleep"],"timeout_seconds":30}},{"id":"second","needs":["first"],"command":{"program":f.request(&[],5).command.program,"args":[]}}]}})).unwrap())}).unwrap();
    let response=client.call_tool(CallToolRequestParams::new("pipeline_cancel").with_arguments(json!({"run_id":selected_pipeline.id,"actor":"user","reason":"Manual selected pipeline cancellation"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let cancelled = finished(&f.engine, &selected_pipeline.id).await;
    assert_eq!(cancelled.status, RunState::Cancelled);
    assert_eq!(cancelled.cancel_actor.as_deref(), Some("user"));
    assert_eq!(
        cancelled.reason.as_deref(),
        Some("Manual selected pipeline cancellation")
    );
    assert!(cancelled.stages.iter().all(|s| s.status.terminal()));
    let response = client
        .call_tool(
            CallToolRequestParams::new("pipeline_cancel").with_arguments(
                json!({"run_id":selected_pipeline.id,"actor":"agent","reason":"Repeated request"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let retained = f.engine.db.run(&selected_pipeline.id).unwrap();
    assert_eq!(retained.cancel_actor.as_deref(), Some("user"));
    assert_eq!(retained.reason, cancelled.reason);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_completion_subscription_delivers_and_replays_by_cursor() {
    use rmcp::{
        ClientLifecycleMode, ClientServiceExt,
        model::{
            ClientConfig, ProtocolVersion, ReadResourceRequestParams, ServerNotification,
            SubscriptionFilter,
        },
    };
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ClientConfig::default()
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp")),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    let uri = preflight::events::uri(&f.project.id);
    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscription(uri.clone())
                .build(),
        )
        .await
        .unwrap();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(3), subscription.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        initial,
        ServerNotification::ResourceUpdatedNotification(_)
    ));
    let run = f.start(&[], 5);
    let update = tokio::time::timeout(std::time::Duration::from_secs(3), subscription.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(update,ServerNotification::ResourceUpdatedNotification(ref n) if n.params.uri==uri)
    );
    let response = client
        .call_tool(
            CallToolRequestParams::new("events_read").with_arguments(
                json!({"project_id":f.project.id,"after":0,"limit":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let events: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(events["events"][0]["run_id"], run.id);
    assert_eq!(events["events"][0]["status"], "succeeded");
    let resource = client
        .read_resource(ReadResourceRequestParams::new(uri.clone()))
        .await
        .unwrap();
    assert!(serde_json::to_string(&resource).unwrap().contains(&run.id));
    subscription.cancel().await.unwrap();
    let replay = client
        .call_tool(
            CallToolRequestParams::new("events_read").with_arguments(
                json!({"project_id":f.project.id,"after":0,"limit":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(response.content, replay.content);
    let (status, rest) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/completion-events?project_id={}&after=0&limit=1",
            f.project.id
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest, events);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[derive(Clone)]
struct CompletionListener(tokio::sync::mpsc::UnboundedSender<String>);
impl rmcp::ClientHandler for CompletionListener {
    async fn on_resource_updated(
        &self,
        params: rmcp::model::ResourceUpdatedNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        let _ = self.0.send(params.uri);
    }
}
#[tokio::test]
#[allow(deprecated)] // Exercise the supported legacy MCP resources/subscribe contract.
async fn legacy_mcp_completion_subscription_stops_on_unsubscribe() {
    use rmcp::{
        ClientLifecycleMode, ClientServiceExt,
        model::{SubscribeRequestParams, UnsubscribeRequestParams},
    };
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let client = CompletionListener(tx)
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp")),
            ClientLifecycleMode::Initialize,
        )
        .await
        .unwrap();
    let uri = preflight::events::uri(&f.project.id);
    client
        .subscribe(SubscribeRequestParams::new(uri.clone()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap(),
        uri
    );
    let run = f.start(&[], 5);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap(),
        uri
    );
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    client
        .unsubscribe(UnsubscribeRequestParams::new(uri.clone()))
        .await
        .unwrap();
    client
        .unsubscribe(UnsubscribeRequestParams::new(uri))
        .await
        .unwrap();
    let second = f.start(&[], 5);
    finished(&f.engine, &second.id).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(450), rx.recv())
            .await
            .is_err()
    );
    client
        .subscribe(SubscribeRequestParams::new(preflight::events::uri(
            &f.project.id,
        )))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while std::sync::Arc::strong_count(&f.engine) > 1 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn retention_mcp_and_rest_share_revisions_and_replay() {
    let f = Fixture::new();
    f.engine
        .db
        .insert_run(
            "retention-producer",
            &StartBuild {
                project_id: f.project.id.clone(),
                command: CommandSpec {
                    program: "tool".into(),
                    args: vec![],
                    timeout_seconds: 5,
                },
            },
            f.temp.path(),
        )
        .unwrap();
    let artifact = Artifact {
        run_id: "retention-producer".into(),
        stage_id: "native".into(),
        name: "module".into(),
        kind: preflight::pipeline::ArtifactKind::File,
        sha256: "b".repeat(64),
        size: 1,
        path: f.temp.path().join("module"),
    };
    f.engine
        .db
        .publish_module(&f.project.id, &artifact, None)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let mut args = json!({"project_id":f.project.id,"sha256":artifact.sha256,"owner":"baseline","pipeline_id":"compiler","pinned":true,"expected_revision":0,"actor":"user","reason":"keep baseline","request_id":"pin"});
    let bound = client
        .call_tool(
            CallToolRequestParams::new("module_pin_set")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(bound.is_error, Some(true));
    let first: Value = serde_json::from_str(&bound.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(first["revision"], 1);
    let (code, replay) = request(&f, "POST", "/api/v1/module-pins", args.clone()).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first, replay);
    args["request_id"] = "stale".into();
    let (code, _) = request(&f, "POST", "/api/v1/module-pins", args.clone()).await;
    assert_eq!(code, StatusCode::CONFLICT);
    args["expected_revision"] = 1.into();
    args["pinned"] = false.into();
    let (code, released) = request(&f, "POST", "/api/v1/module-pins", args).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(released["revision"], 2);
    let query = json!({"project_id":f.project.id,"sha256":artifact.sha256});
    let response = client
        .call_tool(
            CallToolRequestParams::new("module_retention")
                .with_arguments(query.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let report: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(report["pins"][0]["pinned"], false);
    assert_eq!(report["counts"]["publication"], 1);
    let (code, rest) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/module-retention?project_id={}&sha256={}",
            f.project.id, artifact.sha256
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(rest, report);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn change_queue_compact_mcp_responses_and_rest_parity() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let args = json!({"project_id":f.project.id,"name":"changes","request_id":"request","mode":"independent","definition":{"version":1,"pipeline":{"stages":[{"id":"check","command":{"program":"rustc","args":["--version"]}}]}},"changes":[{"id":"base","source":{"commit":"a".repeat(40)}}]});
    let response = client
        .call_tool(
            CallToolRequestParams::new("queue_create")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    assert_eq!(response.content.len(), 1);
    let text = &response.content[0].as_text().unwrap().text;
    assert!(!text.contains('\n'));
    let created: Value = serde_json::from_str(text).unwrap();
    assert_eq!(created.as_object().unwrap().len(), 1);
    let (code, replay) = request(&f, "POST", "/api/v1/change-queues", args).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(replay, created);
    let response = client
        .call_tool(
            CallToolRequestParams::new("queue_status")
                .with_arguments(created.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let compact: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert!(compact["items"][0].get("source").is_none());
    assert_eq!(compact["counts"]["pending"], 1);
    let id = created["queue_id"].as_str().unwrap();
    let (code, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/change-queues/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(compact, rest);
    let (_, detail) = request(
        &f,
        "GET",
        &format!("/api/v1/change-queues/{id}?details=true"),
        Value::Null,
    )
    .await;
    assert_eq!(detail["items"][0]["source"]["commit"], "a".repeat(40));
    let cancel = json!({"queue_id":id,"actor":"user","reason":"cancel queued check"});
    let response = client
        .call_tool(
            CallToolRequestParams::new("queue_cancel")
                .with_arguments(cancel.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    preflight::queues::tick(&f.engine).unwrap();
    assert!(f.engine.db.runs().unwrap().is_empty());
    let response = client
        .call_tool(
            CallToolRequestParams::new("queues_list").with_arguments(
                json!({"project_id":f.project.id,"limit":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let listed: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(listed["queues"][0]["state"], "cancelled");
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn abandoned_operation_resolution_mcp_and_rest_parity() {
    let f = Fixture::new();
    f.engine
        .db
        .insert_run("abandoned-run", &f.request(&[], 5), f.temp.path())
        .unwrap();
    let db = rusqlite::Connection::open(f.temp.path().join("data/preflight.sqlite3")).unwrap();
    db.execute(
        "UPDATE runs SET status='infrastructure_error' WHERE id='abandoned-run'",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO operation_leases VALUES ('lease','abandoned-run','test','abandoned',1,2)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO operation_resources VALUES ('lease','run:abandoned-run')",
        [],
    )
    .unwrap();
    drop(db);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let response = client
        .call_tool(
            CallToolRequestParams::new("operations_abandoned").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let listed: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (code, rest) = request(
        &f,
        "GET",
        &format!("/api/v1/operations-abandoned?project_id={}", f.project.id),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(listed, rest);
    assert_eq!(listed["operations"][0]["lease_id"], "lease");
    let mut args = json!({"project_id":f.project.id,"lease_id":"lease","request_id":"resolve","actor":"test","reason":"no process spawned","process_stopped":false});
    let denied = client
        .call_tool(
            CallToolRequestParams::new("operation_resolve")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));
    args["process_stopped"] = true.into();
    let response = client
        .call_tool(
            CallToolRequestParams::new("operation_resolve")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let resolved: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (code, replay) = request(&f, "POST", "/api/v1/operation-resolve", args).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(resolved, replay);
    assert_eq!(resolved["state"], "released");
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
#[allow(deprecated)] // Keep a legacy completion SSE session open during shutdown.
async fn shutdown_closes_live_mcp_sessions_without_client_disconnect() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(f.engine.clone());
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let engine = f.engine.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                signal.cancelled().await;
                engine.shutdown().await.unwrap();
            })
            .await
            .unwrap();
    });
    use rmcp::{ClientLifecycleMode, ClientServiceExt, model::SubscribeRequestParams};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let client = CompletionListener(tx)
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp")),
            ClientLifecycleMode::Initialize,
        )
        .await
        .unwrap();
    client
        .subscribe(SubscribeRequestParams::new(preflight::events::uri(
            &f.project.id,
        )))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(3), server)
        .await
        .expect("server must drain MCP while the client is still alive")
        .unwrap();
    assert!(f.engine.is_stopping());
    let _ = client.cancel().await;
}

#[tokio::test]
async fn server_bundles_interface_and_keeps_api() {
    let f = Fixture::new();
    for path in [
        "/",
        "/executions",
        "/settings",
        "/projects/id/pipelines",
        "/projects/id/experiments",
    ] {
        let response = router(f.engine.clone())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("Host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("/app.js"));
    }
    let response = router(f.engine.clone())
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .header("Host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (status, caps) = request(&f, "GET", "/api/v1/capabilities", json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        caps["preflight_protocol"]["version"],
        preflight::control::PROTOCOL_VERSION
    );
    assert_eq!(caps["features"]["configured_artifact_ttl"], true);
    assert_eq!(caps["features"]["configured_storage_quotas"], true);
    assert_eq!(caps["features"]["configured_disk_watermarks"], true);
    assert_eq!(caps["service_language"], "en");
    assert_eq!(caps["data_language"], "original");
}

#[tokio::test]
async fn storage_usage_mcp_and_rest_agree() {
    let f = Fixture::new();
    std::fs::write(f.project.root.join("diff.txt"), "captured text\n").unwrap();
    let run = f.start(&[], 5);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let policy_response = client
        .call_tool(CallToolRequestParams::new("storage_policy_status"))
        .await
        .unwrap();
    assert_ne!(policy_response.is_error, Some(true));
    let policy_mcp: Value =
        serde_json::from_str(&policy_response.content[0].as_text().unwrap().text).unwrap();
    let (status, policy_rest) = request(&f, "GET", "/api/v1/storage-policies", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy_rest, policy_mcp);
    assert_eq!(policy_rest["enabled"], false);
    let args = json!({"project_id":f.project.id,"category":"run_workspaces","limit":1});
    let response = client
        .call_tool(
            CallToolRequestParams::new("storage_usage")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let mcp: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (code, rest) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/storage-usage?project_id={}&category=run_workspaces&limit=1",
            f.project.id
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(rest, mcp);
    assert_eq!(rest["entries"][0]["id"], run.id);
    assert_eq!(rest["page_complete"], true);
    let response = client.call_tool(CallToolRequestParams::new("storage_usage").with_arguments(json!({"project_id":f.project.id,"category":"run_workspaces","pipeline_id":"invalid-scope"}).as_object().unwrap().clone())).await.unwrap();
    assert_eq!(response.is_error, Some(true));
    let response = client.call_tool(CallToolRequestParams::new("runs_source_diff").with_arguments(json!({"project_id":f.project.id,"run_id":run.id,"baseline_id":run.id,"path":"diff.txt"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true));
    let mcp_diff: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let (code, rest_diff) = request(
        &f,
        "GET",
        &format!(
            "/api/v1/run-source-diff?project_id={}&run_id={}&baseline_id={}&path=diff.txt",
            f.project.id, run.id, run.id
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(rest_diff, mcp_diff);
    assert_eq!(rest_diff["diff"], "");
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn settings_require_separate_administrative_credential() {
    let f = Fixture::new();
    let (code, _) = request(
        &f,
        "POST",
        "/api/v1/server-settings",
        json!({"admin_token":"invalid"}),
    )
    .await;
    assert!(!code.is_success());
}

#[tokio::test]
async fn stage_retry_mcp_and_http_share_preview_idempotency_and_partial_report() {
    let f = Fixture::new();
    let original = f.start(&[], 5);
    let original = finished(&f.engine, &original.id).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let selection = json!({"project_id":f.project.id,"run_id":original.id,"stage_id":"native"});
    let mcp = client
        .call_tool(
            CallToolRequestParams::new("pipeline_retry_preview")
                .with_arguments(selection.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(mcp.is_error, Some(true));
    let compact: Value = serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
    let (code, http) = request(
        &f,
        "POST",
        "/api/v1/pipeline-retry/preview",
        selection.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(compact, http);
    let args = json!({"selection":selection,"idempotency_key":"one-stage","actor":"agent","reason":"retry selected stage"});
    let mcp = client
        .call_tool(
            CallToolRequestParams::new("pipeline_retry")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(mcp.is_error, Some(true));
    let compact: Value = serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
    let id = compact["run_id"].as_str().unwrap();
    let done = finished(&f.engine, id).await;
    assert_eq!(done.status, RunState::Succeeded);
    assert_eq!(done.stages.len(), 1);
    let (code, http) = request(&f, "POST", "/api/v1/pipeline-retry", args).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    assert_eq!(http["id"], id);
    assert_eq!(http["retry"]["parent_run_id"], original.id);
    let report = preflight::api::compact_report(&f.engine, id).unwrap();
    assert_eq!(report["retry"]["whole_pipeline_verdict"], false);
    let catalog = preflight::inspection::pipelines(
        &f.engine,
        &preflight::inspection::Pipelines {
            project_id: f.project.id.clone(),
        },
    )
    .unwrap();
    assert_eq!(catalog["pipelines"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["pipelines"][0]["runs"], 2);
    assert_eq!(
        serde_json::to_value(f.engine.db.run(&original.id).unwrap()).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
