#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{control, management as m, schedules};
use serde_json::{Value, json};
fn req(v: Value) -> m::Preview {
    serde_json::from_value(v).unwrap()
}
fn exec(f: &common::Fixture, p: &Value) -> m::Execute {
    serde_json::from_value(json!({"project_id":f.project.id,"plan_id":p["plan_id"],"request_id":"reviewed-action","actor":"user","reason":"explicit test approval"})).unwrap()
}
fn save(f: &common::Fixture, name: &str) {
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":name,"expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"sh","args":["-c","printf result > result.txt"]},"outputs":{"result":{"path":"result.txt","type":"file"}}}]}}})).unwrap()).unwrap();
}
fn start(f: &common::Fixture, name: &str) -> preflight::model::Run {
    control::start_saved(
        &f.engine,
        control::ConfigRef {
            project_id: f.project.id.clone(),
            name: name.into(),
            revision: Some(1),
        },
    )
    .unwrap()
}
#[tokio::test]
async fn recoverable_pipeline_and_run_deletion_retains_revisions_and_evidence() {
    let f = common::Fixture::new();
    save(&f, "build");
    let run = start(&f, "build");
    common::finished(&f.engine, &run.id).await;
    let p = m::preview(
        f.engine.clone(),
        req(json!({"project_id":f.project.id,"action":"delete_run","target_id":run.id})),
    )
    .await
    .unwrap();
    let done = m::execute(&f.engine, exec(&f, &p)).unwrap();
    assert_eq!(done["items"][0]["state"], "trashed");
    assert_eq!(m::execute(&f.engine, exec(&f, &p)).unwrap(), done);
    let mut conflicting = exec(&f, &p);
    conflicting.reason = "different".into();
    assert!(m::execute(&f.engine, conflicting).is_err());
    let q = serde_json::from_value(json!({"project_id":f.project.id})).unwrap();
    assert!(
        f.engine.db.query_runs(&q).unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.engine.db.artifacts(&run.id).unwrap().len(), 1);
    let p = m::preview(
        f.engine.clone(),
        req(
            json!({"project_id":f.project.id,"action":"delete_pipeline","target_id":"saved:build"}),
        ),
    )
    .await
    .unwrap();
    m::execute(&f.engine, exec(&f, &p)).unwrap();
    let catalog = || {
        preflight::catalog::list(
            &f.engine,
            &serde_json::from_value(json!({"kind":"saved_pipelines","project_id":f.project.id}))
                .unwrap(),
        )
        .unwrap()
    };
    assert!(catalog()["items"].as_array().unwrap().is_empty());
    assert_eq!(
        f.engine
            .db
            .pipeline_config(&control::ConfigRef {
                project_id: f.project.id.clone(),
                name: "build".into(),
                revision: Some(1)
            })
            .unwrap()["revision"],
        1
    );
    let trash = m::trash(
        &f.engine,
        serde_json::from_value(json!({"project_id":f.project.id})).unwrap(),
    )
    .unwrap();
    assert_eq!(trash["items"].as_array().unwrap().len(), 2);
    for (kind, id) in [("run", run.id.as_str()), ("pipeline", "saved:build")] {
        m::restore(&f.engine,serde_json::from_value(json!({"project_id":f.project.id,"kind":kind,"target_id":id,"actor":"user","reason":"recover"})).unwrap()).unwrap();
    }
    assert_eq!(catalog()["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        f.engine.db.query_runs(&q).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let root = f.temp.path().join("other");
    std::fs::create_dir(&root).unwrap();
    let other = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "other".into(),
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    assert!(
        m::preview(
            f.engine.clone(),
            req(json!({"project_id":other.id,"action":"delete_run","target_id":run.id}))
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn cleanup_rechecks_changed_schedules_and_is_project_scoped() {
    let f = common::Fixture::new();
    save(&f, "build");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let create: schedules::Create=serde_json::from_value(json!({"project_id":f.project.id,"name":"future","pipeline":{"name":"build","revision":1},"at_ms":now+3600000,"grace_ms":60000,"misfire":"run_once","request_id":"future","actor":"user","reason":"test"})).unwrap();
    let row = schedules::create(&f.engine, &create).unwrap();
    let id = row["schedule_id"].as_str().unwrap();
    let p = m::preview(
        f.engine.clone(),
        req(json!({"project_id":f.project.id,"action":"clear_queue"})),
    )
    .await
    .unwrap();
    schedules::change(&f.engine,&serde_json::from_value(json!({"project_id":f.project.id,"schedule_id":id,"action":"pause","expected_revision":1,"request_id":"edit","actor":"user","reason":"edited after preview"})).unwrap()).unwrap();
    assert_eq!(
        m::execute(&f.engine, exec(&f, &p)).unwrap()["items"][0]["state"],
        "skipped_progressed"
    );
    let p = m::preview(
        f.engine.clone(),
        req(json!({"project_id":f.project.id,"action":"clear_queue"})),
    )
    .await
    .unwrap();
    assert_eq!(
        m::execute(&f.engine, exec(&f, &p)).unwrap()["items"][0]["state"],
        "cancelled"
    );
    let actual = schedules::get(
        &f.engine,
        &schedules::Id {
            project_id: f.project.id.clone(),
            schedule_id: id.into(),
        },
    )
    .unwrap();
    assert_eq!(actual["state"], "cancelled");
    assert_eq!(actual["recent_events"][0]["actor"], "user");
}
#[tokio::test]
async fn artifact_cleanup_uses_pipeline_identity_not_shared_cache_id() {
    let f = common::Fixture::new();
    assert!(m::preview(f.engine.clone(),req(json!({"project_id":f.project.id,"action":"artifacts","target_id":"must-not-widen-to-project"}))).await.is_err());
    assert!(m::preview(f.engine.clone(),req(json!({"project_id":f.project.id,"action":"clear_queue","target_id":"must-not-ignore-target"}))).await.is_err());
    save(&f, "first");
    save(&f, "second");
    let a = start(&f, "first");
    let b = start(&f, "second");
    common::finished(&f.engine, &a.id).await;
    common::finished(&f.engine, &b.id).await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let p = m::preview(
        f.engine.clone(),
        req(json!({"project_id":f.project.id,"action":"artifacts","pipeline_key":"saved:first"})),
    )
    .await
    .unwrap();
    assert_eq!(p["entries"].as_array().unwrap().len(), 1);
    assert_eq!(p["entries"][0]["run_id"], a.id);
    assert_eq!(p["entries"][0]["eligible"], true);
    let result=preflight::storage_gc::execute(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"plan_id":p["plan_id"],"request_id":"delete-only-first","actor":"user","reason":"test exact pipeline scope"})).unwrap()).await.unwrap();
    assert!(result["deleted_file_bytes"].as_u64().unwrap() > 0);
    assert_eq!(f.engine.db.artifacts(&b.id).unwrap().len(), 1);
    assert!(f.engine.db.artifacts(&b.id).unwrap()[0].path.exists());
    assert!(!f.engine.db.artifacts(&a.id).unwrap()[0].path.exists());
    assert!(f.engine.db.run(&a.id).unwrap().status.terminal());
}
#[tokio::test]
async fn mcp_and_http_share_preview_execution_and_restore() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    save(&f, "build");
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
    let http = reqwest::Client::new();
    let plan:Value=http.post(format!("http://{address}/api/v1/management/preview")).json(&json!({"project_id":f.project.id,"action":"delete_pipeline","target_id":"saved:build"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let response = client
        .call_tool(
            CallToolRequestParams::new("management_execute").with_arguments(
                serde_json::to_value(exec(&f, &plan))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let trash: Value = http
        .get(format!(
            "http://{address}/api/v1/management/trash?project_id={}",
            f.project.id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(trash["items"][0]["target_id"], "saved:build");
    let response=client.call_tool(CallToolRequestParams::new("management_restore").with_arguments(json!({"project_id":f.project.id,"kind":"pipeline","target_id":"saved:build","actor":"agent","reason":"restore same scoped object"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true));
    let response = client
        .call_tool(
            CallToolRequestParams::new("management_trash").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let response = client
        .call_tool(
            CallToolRequestParams::new("management_preview").with_arguments(
                json!({"project_id":f.project.id,"action":"clear_queue"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    client.cancel().await.unwrap();
    server.abort();
}
