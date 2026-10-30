#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{engine::Engine, storage_gc as gc};
use serde_json::{Value, json};
use std::sync::Arc;
fn sql(e: &Engine) -> rusqlite::Connection {
    rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3")).unwrap()
}
async fn run(e: &Arc<Engine>, project: &str, scope: &str) -> String {
    let run=e.start_pipeline(serde_json::from_value(json!({"project_id":project,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"sh","args":["-c","printf evidence > result.txt; printf intermediate > temporary.txt"],"timeout_seconds":5},"outputs":{"result":{"path":"result.txt","type":"file"}}}]}}})).unwrap()).unwrap();
    assert_eq!(
        common::finished(e, &run.id).await.status,
        preflight::model::RunState::Succeeded
    );
    // Scope is immutable stage metadata; this test does not enable build-cache execution.
    sql(e).execute("UPDATE stage_runs SET definition_json=json_set(definition_json,'$.cache',json_object('pipeline_id',?2,'deterministic',json('true'),'inputs_complete',json('true'))) WHERE run_id=?1",rusqlite::params![run.id,scope]).unwrap();
    run.id
}
async fn preview(e: &Arc<Engine>, project: &str, scope: Option<&str>) -> Value {
    gc::preview(
        e.clone(),
        serde_json::from_value(json!({"project_id":project,"pipeline_id":scope})).unwrap(),
    )
    .await
    .unwrap()
}
fn request(project: &str, plan: &Value) -> gc::Execute {
    serde_json::from_value(json!({"project_id":project,"plan_id":plan["plan_id"],"request_id":format!("gc-{}",plan["plan_id"].as_str().unwrap()),"actor":"user","reason":"reclaim disposable copies"})).unwrap()
}
fn path(e: &Engine, run: &str) -> std::path::PathBuf {
    e.config
        .data_dir
        .join("workspaces")
        .join(run)
        .join("stages/build")
}
#[tokio::test]
async fn scoped_delete_preserves_history_sources_artifacts_and_replays_after_restart() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let a = run(&e, &project.id, "one").await;
    let b = run(&e, &project.id, "two").await;
    let original = std::fs::read(project.root.join("helper")).unwrap();
    let plan = preview(&e, &project.id, Some("one")).await;
    assert_eq!(plan["entries"].as_array().unwrap().len(), 1);
    assert!(plan["estimated_file_bytes"].as_u64().unwrap() > 0);
    let artifacts = e.db.artifacts(&a).unwrap();
    let published = std::fs::read(&artifacts[0].path).unwrap();
    let result = gc::execute(e.clone(), request(&project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["items"][0]["state"], "deleted");
    assert_eq!(result["deleted_file_bytes"], plan["estimated_file_bytes"]);
    assert!(!path(&e, &a).exists());
    assert!(path(&e, &b).exists());
    assert!(
        e.config
            .data_dir
            .join("workspaces")
            .join(&a)
            .join("source/helper")
            .exists()
    );
    assert_eq!(std::fs::read(&artifacts[0].path).unwrap(), published);
    assert_eq!(
        std::fs::read(project.root.join("helper")).unwrap(),
        original
    );
    assert_eq!(
        e.db.run(&a).unwrap().status,
        preflight::model::RunState::Succeeded
    );
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(
        gc::execute(e.clone(), request(&project.id, &plan))
            .await
            .unwrap(),
        result
    );
    let mut changed = request(&project.id, &plan);
    changed.reason = "different".into();
    assert!(gc::execute(e.clone(), changed).await.is_err());
    let mut foreign = request(&project.id, &plan);
    foreign.project_id = uuid::Uuid::now_v7().to_string();
    assert!(gc::execute(e.clone(), foreign).await.is_err());
    drop(e);
    drop(temp);
}
#[tokio::test]
async fn rechecks_new_leases_changed_trees_and_never_follows_symlinks() {
    let f = common::Fixture::new();
    let a = run(&f.engine, &f.project.id, "one").await;
    let plan = preview(&f.engine, &f.project.id, None).await;
    sql(&f.engine)
        .execute(
            "INSERT INTO operation_leases VALUES ('test-lease',?1,'test','active',0,NULL)",
            [&a],
        )
        .unwrap();
    let protected = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(protected["items"][0]["state"], "skipped");
    assert!(path(&f.engine, &a).exists());
    sql(&f.engine)
        .execute(
            "UPDATE operation_leases SET state='released' WHERE id='test-lease'",
            [],
        )
        .unwrap();
    let plan = preview(&f.engine, &f.project.id, None).await;
    std::fs::write(path(&f.engine, &a).join("new-file"), "new").unwrap();
    let changed = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(changed["items"][0]["reason"], "tree changed after preview");
    let outside = f.temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, path(&f.engine, &a).join("link")).unwrap();
    let plan = preview(&f.engine, &f.project.id, None).await;
    assert_eq!(plan["entries"][0]["eligible"], false);
    assert_eq!(plan["estimated_file_bytes"], 0);
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
}
#[tokio::test]
async fn resumes_quarantine_after_restart_without_redeleting_new_source() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let a = run(&e, &project.id, "one").await;
    let plan = preview(&e, &project.id, None).await;
    let request = request(&project.id, &plan);
    let id = plan["plan_id"].as_str().unwrap();
    let quarantine = e.config.data_dir.join("gc-quarantine").join(id);
    std::fs::create_dir_all(&quarantine).unwrap();
    sql(&e)
        .execute(
            "UPDATE storage_gc_plans SET request_id=?2,request_json=?3 WHERE id=?1",
            rusqlite::params![
                id,
                request.request_id,
                serde_json::to_string(&request).unwrap()
            ],
        )
        .unwrap();
    sql(&e)
        .execute(
            "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
            [id],
        )
        .unwrap();
    std::fs::rename(path(&e, &a), quarantine.join("0")).unwrap();
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    let unexpected = quarantine.join("0/unapproved.txt");
    std::fs::write(&unexpected, "not approved").unwrap();
    let interrupted = gc::execute(e.clone(), request.clone()).await.unwrap();
    assert_eq!(interrupted["complete"], false);
    assert!(unexpected.exists());
    assert_eq!(interrupted["deleted_file_bytes"], 0);
    std::fs::remove_file(&unexpected).unwrap();
    let result = gc::execute(e.clone(), request).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert!(!quarantine.join("0").exists());
    assert_eq!(result["deleted_file_bytes"], plan["estimated_file_bytes"]);
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn mcp_preview_http_execute_and_status_share_the_same_plan() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    let run = run(&f.engine, &f.project.id, "one").await;
    sql(&f.engine)
        .execute("UPDATE runs SET finished_at=1 WHERE id=?1", [&run])
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let stop = shutdown.clone();
    let router = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
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
            CallToolRequestParams::new("storage_gc_preview").with_arguments(
                json!({"project_id":f.project.id,"pipeline_id":"one","finished_before":2})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let plan: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(plan["finished_before"], 2);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let result: Value = http
        .post(format!("http://{address}/api/v1/storage-gc-execute"))
        .json(&request(&f.project.id, &plan))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(result["deleted_file_bytes"].as_u64().unwrap() > 0);
    assert!(!path(&f.engine, &run).exists());
    let response = client
        .call_tool(
            CallToolRequestParams::new("storage_gc_status").with_arguments(
                json!({"project_id":f.project.id,"plan_id":plan["plan_id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let status: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(status, result);
    let module = publish(&f.engine, &run);
    let response = client
        .call_tool(
            CallToolRequestParams::new("module_gc_preview").with_arguments(
                json!({"project_id":f.project.id,"pipeline_id":"one"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let plan: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(plan["category"], "module_copies");
    let result: Value = http
        .post(format!("http://{address}/api/v1/storage-gc-execute"))
        .json(&request(&f.project.id, &plan))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "deleted", "{result}");
    assert!(!module.path.exists());
    let plan: Value = http
        .post(format!("http://{address}/api/v1/module-gc-preview"))
        .json(&json!({"project_id":f.project.id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["entries"][0]["eligible"], false);
    let root = f.temp.path().join("expiry-project");
    std::fs::create_dir(&root).unwrap();
    let project = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "expiry".into(),
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let id = run_for_transport_expiry(&f.engine, &project.id).await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let response = client
        .call_tool(
            CallToolRequestParams::new("artifact_expiry_preview").with_arguments(
                json!({"project_id":project.id,"expire_artifacts":true,"finished_before":before})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let plan: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let result: Value = http
        .post(format!("http://{address}/api/v1/storage-gc-execute"))
        .json(&request(&project.id, &plan))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "deleted", "{result}");
    let response = client
        .call_tool(
            CallToolRequestParams::new("artifacts_list")
                .with_arguments(json!({"run_id":id}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let artifacts: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(artifacts[0]["availability"], "artifact_expired");
    client.cancel().await.unwrap();
    shutdown.cancel();
    server.await.unwrap();
}

async fn module_plan(e: &Arc<Engine>, project: &str, scope: Option<&str>) -> Value {
    preflight::module_gc::preview(
        e.clone(),
        serde_json::from_value(json!({"project_id":project,"pipeline_id":scope})).unwrap(),
    )
    .await
    .unwrap()
}
fn publish(e: &Engine, run: &str) -> preflight::model::Artifact {
    preflight::modules::publish(
        e,
        &preflight::modules::Publish {
            run_id: run.into(),
            stage_id: "build".into(),
            artifact_name: "result".into(),
        },
    )
    .unwrap()
}
#[tokio::test]
async fn module_copy_eviction_preserves_producer_and_can_republish() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let a = publish(&f.engine, &id);
    let original = f.engine.db.artifacts(&id).unwrap()[0].clone();
    let other_root = f.temp.path().join("other-project");
    std::fs::create_dir(&other_root).unwrap();
    let other = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "other".into(),
            path: other_root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let other_run = run(&f.engine, &other.id, "one").await;
    let other_module = publish(&f.engine, &other_run);
    assert_eq!(a.sha256, other_module.sha256);

    let plan = module_plan(&f.engine, &f.project.id, Some("one")).await;
    assert_eq!(plan["entries"][0]["eligible"], true, "{plan}");
    let req = request(&f.project.id, &plan);
    let result = gc::execute(f.engine.clone(), req.clone()).await.unwrap();
    assert_eq!(result["category"], "module_copies");
    assert_eq!(result["items"][0]["state"], "deleted", "{result}");
    assert!(!a.path.exists());
    assert!(other_module.path.exists());
    assert_eq!(std::fs::read(&original.path).unwrap(), b"evidence");
    assert!(
        f.engine
            .db
            .module(&f.project.id, &a.sha256)
            .unwrap_err()
            .to_string()
            .contains("evicted")
    );
    assert_eq!(
        f.engine.db.run(&id).unwrap().status,
        preflight::model::RunState::Succeeded
    );
    assert_eq!(
        gc::execute(f.engine.clone(), req.clone()).await.unwrap(),
        result
    );
    let restored = publish(&f.engine, &id);
    assert_eq!(std::fs::read(restored.path).unwrap(), b"evidence");
    // Replaying the old request never deletes a new publication.
    assert_eq!(gc::execute(f.engine.clone(), req).await.unwrap(), result);
    assert!(a.path.exists());
}
#[tokio::test]
async fn module_gc_rechecks_new_pins_and_protects_shared_scope_and_leases() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let a = publish(&f.engine, &id);
    let plan = module_plan(&f.engine, &f.project.id, Some("one")).await;
    f.engine.db.module_pin_set(&serde_json::from_value(json!({"project_id":f.project.id,"sha256":a.sha256,"owner":"baseline","pipeline_id":"one","pinned":true,"expected_revision":0,"actor":"user","reason":"keep","request_id":"pin"})).unwrap()).unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(a.path.exists());
    f.engine.db.module_pin_set(&serde_json::from_value(json!({"project_id":f.project.id,"sha256":a.sha256,"owner":"baseline","pipeline_id":"one","pinned":false,"expected_revision":1,"actor":"user","reason":"release","request_id":"unpin"})).unwrap()).unwrap();
    let other = run(&f.engine, &f.project.id, "two").await;
    publish(&f.engine, &other);
    // Identical content published by a second pipeline still belongs to its scope.
    for scope in ["one", "two"] {
        let usage = preflight::storage_usage::usage(
            f.engine.clone(),
            serde_json::from_value(
                json!({"project_id":f.project.id,"pipeline_id":scope,"category":"module_copies"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(usage["entries"].as_array().unwrap().len(), 1);
        assert_eq!(usage["entries"][0]["id"], a.sha256);
        let shared = module_plan(&f.engine, &f.project.id, Some(scope)).await;
        assert_eq!(shared["entries"].as_array().unwrap().len(), 1);
        assert_eq!(shared["entries"][0]["eligible"], false);
    }
    let plan = module_plan(&f.engine, &f.project.id, Some("one")).await;
    assert_eq!(plan["entries"][0]["eligible"], false);
    assert!(
        plan["entries"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("shared")
    );
    let plan = module_plan(&f.engine, &f.project.id, None).await;
    assert_eq!(plan["entries"][0]["eligible"], true);
    let c = sql(&f.engine);
    c.execute(
        "INSERT INTO operation_leases VALUES ('hold',?1,'test','abandoned',0,NULL)",
        [&id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO operation_resources VALUES ('hold',?1)",
        [format!("module:{}", a.sha256)],
    )
    .unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(a.path.exists());
}
#[tokio::test]
async fn module_quarantine_recovery_does_not_remove_republished_copy() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "one").await;
    let a = publish(&e, &id);
    let plan = module_plan(&e, &project.id, None).await;
    let req = request(&project.id, &plan);
    let plan_id = plan["plan_id"].as_str().unwrap();
    let target = e.config.data_dir.join("gc-quarantine").join(plan_id);
    std::fs::create_dir_all(&target).unwrap();
    let c = sql(&e);
    c.execute(
        "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
        [plan_id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO module_evictions VALUES (?1,?2,?3,0)",
        rusqlite::params![project.id, a.sha256, plan_id],
    )
    .unwrap();
    std::fs::rename(a.path.parent().unwrap(), target.join("0")).unwrap();
    drop(c);
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    publish(&e, &id);
    let result = gc::execute(e.clone(), req).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted", "{result}");
    assert!(!target.join("0").exists());
    assert_eq!(std::fs::read(&a.path).unwrap(), b"evidence");
    assert!(e.db.module(&project.id, &a.sha256).is_ok());
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn republish_supersedes_unmoved_intent_and_consumers_protect_copy() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let a = publish(&f.engine, &id);
    let plan = module_plan(&f.engine, &f.project.id, None).await;
    let plan_id = plan["plan_id"].as_str().unwrap();
    let c = sql(&f.engine);
    c.execute(
        "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
        [plan_id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO module_evictions VALUES (?1,?2,?3,0)",
        rusqlite::params![f.project.id, a.sha256, plan_id],
    )
    .unwrap();
    assert!(f.engine.db.module_pin_set(&serde_json::from_value(json!({"project_id":f.project.id,"sha256":a.sha256,"owner":"baseline","pinned":true,"expected_revision":0,"actor":"user","reason":"keep","request_id":"pin"})).unwrap()).is_err());
    publish(&f.engine, &id);
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped", "{result}");
    assert!(a.path.exists());
    c.execute(
        "INSERT INTO module_inputs VALUES (?1,'build','copied-input',?2)",
        rusqlite::params![id, serde_json::to_string(&a).unwrap()],
    )
    .unwrap();
    let plan = module_plan(&f.engine, &f.project.id, None).await;
    assert_eq!(plan["entries"][0]["eligible"], false);
    assert!(
        plan["entries"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("consumer")
    );
}

#[tokio::test]
async fn module_gc_keeps_last_good_copy_if_producer_corrupts_after_preview() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let a = publish(&f.engine, &id);
    let plan = module_plan(&f.engine, &f.project.id, None).await;
    let original = f.engine.db.artifacts(&id).unwrap()[0].clone();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&original.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(original.path, "corrupt").unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(
        result["items"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("verified")
    );
    assert_eq!(std::fs::read(a.path).unwrap(), b"evidence");
}

async fn expiry_plan(e: &Arc<Engine>, project: &str, scope: Option<&str>) -> Value {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    preflight::artifact_expiry::preview(e.clone(),serde_json::from_value(json!({"project_id":project,"pipeline_id":scope,"expire_artifacts":true,"finished_before":before})).unwrap()).await.unwrap()
}
#[tokio::test]
async fn explicit_artifact_expiry_retains_history_and_reports_expired_after_restart() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let a = run(&e, &project.id, "one").await;
    let b = run(&e, &project.id, "two").await;
    let artifacts = e.db.artifacts(&a).unwrap();
    let second = e.db.artifacts(&b).unwrap();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(
        preflight::artifact_expiry::preview(
            e.clone(),
            serde_json::from_value(
                json!({"project_id":project.id,"expire_artifacts":false,"finished_before":before})
            )
            .unwrap()
        )
        .await
        .is_err()
    );
    let plan = expiry_plan(&e, &project.id, Some("one")).await;
    assert_eq!(plan["entries"].as_array().unwrap().len(), 1);
    let req = request(&project.id, &plan);
    let result = gc::execute(e.clone(), req.clone()).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted", "{result}");
    assert!(!artifacts[0].path.exists());
    assert!(second[0].path.exists());
    let report = e.db.artifacts_report(&a).unwrap();
    assert_eq!(report[0]["availability"], "artifact_expired");
    assert_eq!(report[0]["sha256"], artifacts[0].sha256);
    assert_eq!(
        e.db.run(&a).unwrap().status,
        preflight::model::RunState::Succeeded
    );
    assert!(
        preflight::modules::publish(
            &e,
            &preflight::modules::Publish {
                run_id: a.clone(),
                stage_id: "build".into(),
                artifact_name: "result".into()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("artifact_expired")
    );
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(
        e.db.artifacts_report(&a).unwrap()[0]["availability"],
        "artifact_expired"
    );
    assert_eq!(gc::execute(e.clone(), req).await.unwrap(), result);
    drop(e);
    drop(temp);
}
#[tokio::test]
async fn artifact_expiry_rechecks_new_module_cache_and_experiment_references() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let plan = expiry_plan(&f.engine, &f.project.id, None).await;
    let c = sql(&f.engine);
    c.execute(
        "INSERT INTO stage_cache VALUES ('entry',?1,'one','key',?2,'build','active')",
        rusqlite::params![f.project.id, id],
    )
    .unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    c.execute("UPDATE stage_cache SET state='invalidated'", [])
        .unwrap();
    let plan = expiry_plan(&f.engine, &f.project.id, None).await;
    publish(&f.engine, &id);
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    let other = common::Fixture::new();
    let baseline = run(&other.engine, &other.project.id, "one").await;
    let plan = expiry_plan(&other.engine, &other.project.id, None).await;
    sql(&other.engine)
        .execute(
            "INSERT INTO experiment_runs VALUES (?1,?2,'trial',1,'{}',?1)",
            rusqlite::params![baseline, other.project.id],
        )
        .unwrap();
    let result = gc::execute(other.engine.clone(), request(&other.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
}
#[tokio::test]
async fn artifact_expiry_recovers_quarantine_and_keeps_explicit_marker() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let plan = expiry_plan(&f.engine, &f.project.id, None).await;
    let plan_id = plan["plan_id"].as_str().unwrap();
    let c = sql(&f.engine);
    c.execute(
        "INSERT INTO artifact_expiry VALUES (?1,'build',?2,0)",
        rusqlite::params![id, plan_id],
    )
    .unwrap();
    c.execute(
        "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
        [plan_id],
    )
    .unwrap();
    let target = f.engine.config.data_dir.join("gc-quarantine").join(plan_id);
    std::fs::create_dir_all(&target).unwrap();
    let artifact = f.engine.db.artifacts(&id).unwrap()[0].clone();
    std::fs::rename(artifact.path.parent().unwrap(), target.join("0")).unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert!(!target.join("0").exists());
    assert_eq!(
        f.engine.db.artifacts_report(&id).unwrap()[0]["availability"],
        "artifact_expired"
    );
}

async fn run_for_transport_expiry(e: &Arc<Engine>, project: &str) -> String {
    let id = run(e, project, "one").await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    id
}
#[tokio::test]
async fn saved_experiment_baseline_blocks_an_already_previewed_expiry() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let plan = expiry_plan(&f.engine, &f.project.id, None).await;
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":"checks","expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"sh","args":["-c","true"],"timeout_seconds":5}}]}}})).unwrap()).unwrap();
    preflight::experiments::save(&f.engine,&serde_json::from_value(json!({"project_id":f.project.id,"name":"trial","expected_revision":0,"definition":{"version":1,"pipeline":{"name":"checks","revision":1},"baseline_run":id}})).unwrap()).unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(
        result["items"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("saved experiment")
    );
}

fn pin_request(project: &str, run: &str) -> preflight::artifact_retention::Pin {
    serde_json::from_value(json!({"project_id":project,"run_id":run,"stage_id":"build","artifact_name":"result","owner":"release","pinned":true,"expected_revision":0,"actor":"user","reason":"preserve release output","request_id":"pin-release"})).unwrap()
}
#[tokio::test]
async fn artifact_pin_rechecks_preview_and_exact_replay_never_repin_after_release() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "one").await;
    let plan = expiry_plan(&e, &project.id, None).await;
    let pin = pin_request(&project.id, &id);
    let saved = e.db.artifact_pin_set(&pin).unwrap();
    assert_eq!(saved["revision"], 1);
    let skipped = gc::execute(e.clone(), request(&project.id, &plan))
        .await
        .unwrap();
    assert_eq!(skipped["items"][0]["state"], "skipped");
    let mut unpin = pin.clone();
    unpin.request_id = "release-pin".into();
    unpin.expected_revision = 1;
    unpin.pinned = false;
    assert_eq!(e.db.artifact_pin_set(&unpin).unwrap()["revision"], 2);
    assert_eq!(e.db.artifact_pin_set(&pin).unwrap(), saved);
    let mut changed = pin.clone();
    changed.reason = "altered replay".into();
    assert!(e.db.artifact_pin_set(&changed).is_err());
    let query: preflight::artifact_retention::Query = serde_json::from_value(
        json!({"project_id":project.id,"run_id":id,"stage_id":"build","artifact_name":"result"}),
    )
    .unwrap();
    assert_eq!(e.db.artifact_retention(&query).unwrap()["active_owners"], 0);
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(e.db.artifact_pin_set(&pin).unwrap(), saved);
    let plan = expiry_plan(&e, &project.id, None).await;
    let result = gc::execute(e.clone(), request(&project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    let mut repin = pin.clone();
    repin.request_id = "repin".into();
    repin.expected_revision = 2;
    assert!(e.db.artifact_pin_set(&repin).is_err());
    assert_eq!(
        e.db.artifact_retention(&query).unwrap()["availability"],
        "artifact_expired"
    );
    drop(e);
    drop(temp);
}
#[tokio::test]
async fn artifact_pins_isolate_projects_page_owners_and_reject_competing_revisions() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
    let pin = pin_request(&f.project.id, &id);
    let mut wrong = pin.clone();
    wrong.project_id = "wrong".into();
    assert!(f.engine.db.artifact_pin_set(&wrong).is_err());
    let mut other = pin.clone();
    other.request_id = "competing".into();
    let e = f.engine.clone();
    let a = tokio::task::spawn_blocking(move || e.db.artifact_pin_set(&pin));
    let e = f.engine.clone();
    let b = tokio::task::spawn_blocking(move || e.db.artifact_pin_set(&other));
    let results = [a.await.unwrap(), b.await.unwrap()];
    assert_eq!(results.iter().filter(|v| v.is_ok()).count(), 1);
    for n in 0..100 {
        let mut p = pin_request(&f.project.id, &id);
        p.owner = format!("owner-{n:03}");
        p.request_id = p.owner.clone();
        f.engine.db.artifact_pin_set(&p).unwrap();
    }
    let query = |offset| {
        serde_json::from_value(json!({"project_id":f.project.id,"run_id":id,"stage_id":"build","artifact_name":"result","offset":offset})).unwrap()
    };
    let first = f.engine.db.artifact_retention(&query(0)).unwrap();
    assert_eq!(first["total"], 101);
    assert_eq!(first["owners"].as_array().unwrap().len(), 100);
    assert_eq!(first["next_cursor"], 100);
    let last = f.engine.db.artifact_retention(&query(100)).unwrap();
    assert_eq!(last["owners"].as_array().unwrap().len(), 1);
    assert!(last["next_cursor"].is_null());
}
#[tokio::test]
async fn artifact_pins_mcp_and_http_share_owner_revision() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one").await;
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
    let pin = pin_request(&f.project.id, &id);
    let response = client
        .call_tool(
            CallToolRequestParams::new("artifact_pin_set").with_arguments(
                serde_json::to_value(&pin)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let retained: Value = http
        .get(format!("http://{address}/api/v1/artifact-retention?project_id={}&run_id={id}&stage_id=build&artifact_name=result",f.project.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retained["active_owners"], 1);
    let mut release = pin;
    release.pinned = false;
    release.expected_revision = 1;
    release.request_id = "http-release".into();
    let result: Value = http
        .post(format!("http://{address}/api/v1/artifact-pins"))
        .json(&release)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["revision"], 2);
    let response=client.call_tool(CallToolRequestParams::new("artifact_retention").with_arguments(json!({"project_id":f.project.id,"run_id":id,"stage_id":"build","artifact_name":"result"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true));
    let result: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(result["active_owners"], 0);
    assert_eq!(result["owners"][0]["revision"], 2);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn usage_is_scoped_paginated_and_read_only() {
    let common::Fixture {
        engine: e,
        project,
        temp: _temp,
    } = common::Fixture::new();
    let a = run(&e, &project.id, "one").await;
    let b = run(&e, &project.id, "two").await;
    let query = |scope: Option<&str>, before: Option<&str>, limit: usize| {
        serde_json::from_value(json!({"project_id":project.id,"category":"stage_workspaces","pipeline_id":scope,"before":before,"limit":limit})).unwrap()
    };
    let scoped = preflight::storage_usage::usage(e.clone(), query(Some("one"), None, 20))
        .await
        .unwrap();
    assert_eq!(scoped["entries"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["entries"][0]["id"], format!("{a}/build"));
    assert_eq!(scoped["page_complete"], true);
    assert!(scoped["page_file_bytes"].as_u64().unwrap() > 0);
    let first = preflight::storage_usage::usage(e.clone(), query(None, None, 1))
        .await
        .unwrap();
    let second =
        preflight::storage_usage::usage(e.clone(), query(None, first["next_cursor"].as_str(), 1))
            .await
            .unwrap();
    assert_ne!(first["entries"][0]["id"], second["entries"][0]["id"]);
    assert!(second["next_cursor"].is_null());
    let absent = preflight::storage_usage::usage(e.clone(), query(Some("other"), None, 20))
        .await
        .unwrap();
    assert!(absent["entries"].as_array().unwrap().is_empty());
    assert!(path(&e, &a).exists());
    assert!(path(&e, &b).exists());
    assert!(
        preflight::storage_usage::usage(e.clone(), query(None, None, 0))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retention_cutoff_is_scoped_exclusive_and_rechecked() {
    let f = common::Fixture::new();
    let old = run(&f.engine, &f.project.id, "one").await;
    let recent = run(&f.engine, &f.project.id, "one").await;
    let other = run(&f.engine, &f.project.id, "two").await;
    let cutoff = 1_700_000_000_000i64;
    let c = sql(&f.engine);
    for id in [&old, &other] {
        c.execute(
            "UPDATE runs SET finished_at=?2 WHERE id=?1",
            rusqlite::params![id, cutoff - 1],
        )
        .unwrap();
    }
    c.execute(
        "UPDATE runs SET finished_at=?2 WHERE id=?1",
        rusqlite::params![recent, cutoff],
    )
    .unwrap();
    let query = || {
        serde_json::from_value(
            json!({"project_id":f.project.id,"pipeline_id":"one","finished_before":cutoff}),
        )
        .unwrap()
    };
    let plan = gc::preview(f.engine.clone(), query()).await.unwrap();
    assert_eq!(plan["finished_before"], cutoff);
    assert_eq!(plan["entries"].as_array().unwrap().len(), 1);
    assert_eq!(plan["entries"][0]["run_id"], old);
    c.execute(
        "UPDATE runs SET finished_at=?2 WHERE id=?1",
        rusqlite::params![old, cutoff + 1],
    )
    .unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(
        result["items"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("retention cutoff")
    );
    assert!(path(&f.engine, &old).exists());
    assert!(path(&f.engine, &recent).exists());
    assert!(path(&f.engine, &other).exists());
    c.execute(
        "UPDATE runs SET finished_at=?2 WHERE id=?1",
        rusqlite::params![old, cutoff - 1],
    )
    .unwrap();
    let plan = gc::preview(f.engine.clone(), query()).await.unwrap();
    let req = request(&f.project.id, &plan);
    let result = gc::execute(f.engine.clone(), req.clone()).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert_eq!(result, gc::execute(f.engine.clone(), req).await.unwrap());
    assert!(
        f.engine
            .db
            .run(&old)
            .unwrap()
            .workspace
            .join("source")
            .exists()
    );
    assert!(!f.engine.db.artifacts(&old).unwrap().is_empty());
    for cutoff in [0, i64::MAX] {
        assert!(
            gc::preview(
                f.engine.clone(),
                serde_json::from_value(json!({"project_id":f.project.id,"finished_before":cutoff}))
                    .unwrap()
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn configured_ttl_is_scoped_bounded_and_resumes_after_restart() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    assert_eq!(policy::status(&e).unwrap()["enabled"], false);
    let first = run(&e, &project.id, "ttl-one").await;
    let second = run(&e, &project.id, "ttl-one").await;
    let foreign = run(&e, &project.id, "ttl-two").await;
    let fresh = run(&e, &project.id, "ttl-one").await;
    let conn = sql(&e);
    for id in [&first, &second, &foreign] {
        conn.execute(
            "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
            [id],
        )
        .unwrap();
    }
    let expected_artifact = e.db.artifacts(&first).unwrap()[0].clone();
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        disk_pressure: None,
        name: "stage-ttl".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("ttl-one".into()),
        quota: None,
        category: policy::Category::StageWorkspaces,
        ttl_seconds: 60,
        interval_seconds: 3600,
        batch_limit: 1,
    }];
    drop(conn);
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "active");
    let cutoff = cycle["finished_before"].clone();
    assert!(!path(&e, &second).exists());
    assert!(path(&e, &first).exists());
    assert!(path(&e, &foreign).exists());
    assert!(path(&e, &fresh).exists());
    assert!(expected_artifact.path.exists());
    assert!(
        e.config
            .data_dir
            .join("workspaces")
            .join(&first)
            .join("source")
            .exists()
    );
    // Simulate interruption after execute but before advancing the policy cursor.
    let old_plan: String = sql(&e)
        .query_row(
            "SELECT id FROM storage_gc_plans ORDER BY created_at DESC,id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    sql(&e).execute("UPDATE storage_policy_cycles SET cursor=NULL,plan_id=?2,next_cursor=(SELECT json_extract(preview_json,'$.next_cursor') FROM storage_gc_plans WHERE id=?2) WHERE id=?1",rusqlite::params![cycle["cycle_id"].as_str().unwrap(),old_plan]).unwrap();
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    assert!(path(&e, &first).exists());
    assert_eq!(
        sql(&e)
            .query_row("SELECT COUNT(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let last = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(last["state"], "complete");
    assert_eq!(last["finished_before"], cutoff);
    assert!(!path(&e, &first).exists());
    assert!(path(&e, &foreign).exists());
    assert!(path(&e, &fresh).exists());
    assert!(expected_artifact.path.exists());
    assert_eq!(e.db.runs().unwrap().len(), 4);
    let count: i64 = sql(&e)
        .query_row("SELECT COUNT(*) FROM storage_gc_plans", [], |r| r.get(0))
        .unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(
        sql(&e)
            .query_row("SELECT COUNT(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    e.shutdown().await.unwrap();
    drop(e);
    config.storage_policies.clear();
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(policy::status(&e).unwrap()["enabled"], false);
    assert!(path(&e, &foreign).exists());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn artifact_ttl_resumes_fixed_cutoff_keeps_pins_history_and_other_scopes() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let first = run(&e, &project.id, "artifact-ttl").await;
    let pinned = run(&e, &project.id, "artifact-ttl").await;
    let foreign = run(&e, &project.id, "other-pipeline").await;
    let fresh = run(&e, &project.id, "artifact-ttl").await;
    let failed = run(&e, &project.id, "artifact-ttl").await;
    e.db.artifact_pin_set(&pin_request(&project.id, &pinned))
        .unwrap();
    let conn = sql(&e);
    for id in [&first, &pinned, &foreign, &failed] {
        conn.execute(
            "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
            [id],
        )
        .unwrap();
    }
    conn.execute("UPDATE runs SET status='failed' WHERE id=?1", [&failed])
        .unwrap();
    let artifact = e.db.artifacts(&first).unwrap()[0].clone();
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        disk_pressure: None,
        name: "artifact-ttl".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("artifact-ttl".into()),
        quota: None,
        category: policy::Category::ArtifactExpiry,
        ttl_seconds: 60,
        interval_seconds: 3600,
        batch_limit: 1,
    }];
    e.shutdown().await.unwrap();
    drop(conn);
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "active");
    let cutoff = cycle["finished_before"].clone();
    let old_plan = cycle["plan_id"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| {
            sql(&e)
                .query_row(
                    "SELECT id FROM storage_gc_plans ORDER BY created_at DESC,id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        });
    // Replay a completed page after a crash before the durable cursor advanced.
    sql(&e).execute("UPDATE storage_policy_cycles SET cursor=NULL,plan_id=?2,next_cursor=(SELECT json_extract(preview_json,'$.next_cursor') FROM storage_gc_plans WHERE id=?2) WHERE id=?1",rusqlite::params![cycle["cycle_id"].as_str().unwrap(),old_plan]).unwrap();
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    for _ in 0..6 {
        policy::tick(&e).await.unwrap();
        let status = policy::status(&e).unwrap();
        let cycle = &status["policies"][0]["recent_cycles"][0];
        assert_eq!(cycle["finished_before"], cutoff);
        if cycle["state"] == "complete" {
            break;
        }
    }
    assert_eq!(
        policy::status(&e).unwrap()["policies"][0]["recent_cycles"][0]["state"],
        "complete"
    );
    assert!(!artifact.path.exists());
    assert_eq!(
        e.db.artifacts_report(&first).unwrap()[0]["availability"],
        "artifact_expired"
    );
    for id in [&pinned, &foreign, &fresh, &failed] {
        assert!(
            e.db.artifacts(id).unwrap()[0].path.exists(),
            "protected artifact removed: {id}"
        );
        assert_eq!(
            e.db.artifacts_report(id).unwrap()[0]["availability"],
            "available"
        );
    }
    assert_eq!(e.db.runs().unwrap().len(), 5);
    assert!(path(&e, &first).exists());
    assert!(
        e.config
            .data_dir
            .join("workspaces")
            .join(&first)
            .join("source")
            .exists()
    );
    let count = sql(&e)
        .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    e.shutdown().await.unwrap();
    drop(e);
    config.storage_policies.clear();
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(policy::status(&e).unwrap()["enabled"], false);
    assert!(e.db.artifacts(&pinned).unwrap()[0].path.exists());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn quota_watermarks_resume_hysteresis_and_stop_before_deleting_remaining_data() {
    use preflight::{storage_policy as policy, storage_usage as usage};
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let a = run(&e, &project.id, "quota").await;
    let b = run(&e, &project.id, "quota").await;
    let c = run(&e, &project.id, "quota").await;
    let foreign = run(&e, &project.id, "foreign").await;
    for id in [&a, &b, &c, &foreign] {
        sql(&e)
            .execute(
                "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
                [id],
            )
            .unwrap();
    }
    let measured = usage::usage(
        e.clone(),
        serde_json::from_value(
            json!({"project_id":project.id,"pipeline_id":"quota","category":"stage_workspaces"}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(measured["page_complete"], true);
    let total = measured["page_file_bytes"].as_u64().unwrap();
    let keep = measured["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == format!("{a}/build"))
        .unwrap()["observed_file_bytes"]
        .as_u64()
        .unwrap();
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        disk_pressure: None,
        name: "quota".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("quota".into()),
        category: policy::Category::StageWorkspaces,
        ttl_seconds: 60,
        interval_seconds: 3600,
        batch_limit: 1,
        quota: Some(policy::Quota {
            high_bytes: total + 1,
            low_bytes: keep,
        }),
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(path(&e, &c).exists());
    assert_eq!(
        policy::status(&e).unwrap()["policies"][0]["recent_cycles"][0]["quota_observation"]["triggered"],
        false
    );
    e.shutdown().await.unwrap();
    drop(e);
    config.storage_policies[0]
        .quota
        .as_mut()
        .unwrap()
        .high_bytes = total - 1;
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "active");
    assert_eq!(cycle["quota_observation"]["triggered"], true);
    assert!(
        cycle["quota_observation"]["observed_file_bytes"]
            .as_u64()
            .unwrap()
            < total - 1
    );
    assert!(!path(&e, &c).exists());
    assert!(path(&e, &b).exists());
    let cutoff = cycle["finished_before"].clone();
    let old_plan: String = sql(&e)
        .query_row(
            "SELECT id FROM storage_gc_plans ORDER BY created_at DESC,id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    sql(&e).execute("UPDATE storage_policy_cycles SET cursor=NULL,plan_id=?2,next_cursor=(SELECT json_extract(preview_json,'$.next_cursor') FROM storage_gc_plans WHERE id=?2) WHERE id=?1",rusqlite::params![cycle["cycle_id"].as_str().unwrap(),old_plan]).unwrap();
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    assert!(path(&e, &b).exists());
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "complete");
    assert_eq!(cycle["finished_before"], cutoff);
    assert_eq!(cycle["quota_observation"]["complete"], true);
    assert_eq!(cycle["quota_observation"]["observed_file_bytes"], keep);
    assert!(!path(&e, &b).exists());
    assert!(path(&e, &a).exists());
    assert!(path(&e, &foreign).exists());
    for id in [&a, &b, &c, &foreign] {
        assert!(e.db.artifacts(id).unwrap()[0].path.exists());
    }
    assert_eq!(e.db.runs().unwrap().len(), 4);
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn artifact_quota_reports_protected_target_and_incomplete_measurement_without_removal() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "quota-protected").await;
    e.db.artifact_pin_set(&pin_request(&project.id, &id))
        .unwrap();
    sql(&e)
        .execute(
            "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        disk_pressure: None,
        name: "protected-quota".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("quota-protected".into()),
        category: policy::Category::ArtifactExpiry,
        ttl_seconds: 60,
        interval_seconds: 60,
        batch_limit: 1,
        quota: Some(policy::Quota {
            high_bytes: 1,
            low_bytes: 0,
        }),
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "failed");
    assert!(
        cycle["error"]
            .as_str()
            .unwrap()
            .contains("quota target unmet")
    );
    assert_eq!(cycle["quota_observation"]["observed_file_bytes"], 8);
    let artifact = e.db.artifacts(&id).unwrap()[0].clone();
    assert!(artifact.path.exists());
    e.shutdown().await.unwrap();
    drop(e);
    config.storage_policies[0].name = "linked-quota".into();
    let e = Engine::open(config).unwrap();
    let link = artifact.path.parent().unwrap().join("unsafe-link");
    std::os::unix::fs::symlink(project.root.join("helper"), &link).unwrap();
    let plans = sql(&e)
        .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "failed");
    assert!(
        cycle["error"]
            .as_str()
            .unwrap()
            .contains("measurement incomplete")
    );
    assert_eq!(cycle["quota_observation"]["complete"], false);
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        plans
    );
    assert!(artifact.path.exists());
    assert!(link.is_symlink());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn artifact_quota_expires_only_output_bytes_and_reaches_zero_target() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "quota-artifact").await;
    sql(&e)
        .execute(
            "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        disk_pressure: None,
        name: "outputs-quota".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("quota-artifact".into()),
        category: policy::Category::ArtifactExpiry,
        ttl_seconds: 60,
        interval_seconds: 60,
        batch_limit: 1,
        quota: Some(policy::Quota {
            high_bytes: 1,
            low_bytes: 0,
        }),
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "complete");
    assert_eq!(cycle["quota_observation"]["observed_file_bytes"], 0);
    assert_eq!(
        e.db.artifacts_report(&id).unwrap()[0]["availability"],
        "artifact_expired"
    );
    assert!(path(&e, &id).exists());
    assert!(e.db.run(&id).unwrap().status.terminal());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn disk_pressure_reads_real_filesystem_and_refuses_impossible_target_without_deletion() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "disk").await;
    sql(&e)
        .execute(
            "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let stats = fs2::statvfs(&e.config.data_dir).unwrap();
    assert!(stats.available_space() > 2);
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        name: "disk".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("disk".into()),
        category: policy::Category::StageWorkspaces,
        ttl_seconds: 60,
        interval_seconds: 60,
        batch_limit: 1,
        quota: None,
        disk_pressure: Some(policy::DiskPressure {
            start_below_bytes: 1,
            stop_at_bytes: 2,
        }),
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "complete");
    assert!(cycle["quota_observation"].is_null());
    assert_eq!(cycle["disk_observation"]["complete"], true);
    assert!(cycle["disk_observation"]["total_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(path(&e, &id).exists());
    e.shutdown().await.unwrap();
    drop(e);
    config.storage_policies[0]
        .disk_pressure
        .as_mut()
        .unwrap()
        .stop_at_bytes = stats.total_space() + 1;
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["state"], "failed");
    assert!(
        cycle["error"]
            .as_str()
            .unwrap()
            .contains("exceeds filesystem capacity")
    );
    assert_eq!(
        sql(&e)
            .query_row("SELECT count(*) FROM storage_gc_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(path(&e, &id).exists());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
#[ignore = "requires stable filesystem with at least 64 MiB occupied; explicit real-volume watermark acceptance"]
async fn actual_disk_space_pressure_reclaims_only_owned_fixture_scope() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let id = run(&e, &project.id, "disk-pressure").await;
    let other = run(&e, &project.id, "other-scope").await;
    for run in [&id, &other] {
        sql(&e)
            .execute(
                "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
                [run],
            )
            .unwrap();
    }
    let stats = fs2::statvfs(&e.config.data_dir).unwrap();
    let delta = (stats.total_space() - stats.available_space()) / 4;
    assert!(
        delta > 16 * 1024 * 1024,
        "fixture needs enough occupied space to avoid sensitivity to small external writes"
    );
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        name: "disk-pressure".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("disk-pressure".into()),
        category: policy::Category::StageWorkspaces,
        ttl_seconds: 60,
        interval_seconds: 60,
        batch_limit: 1,
        quota: None,
        disk_pressure: Some(policy::DiskPressure {
            start_below_bytes: stats.available_space() + delta,
            stop_at_bytes: stats.available_space() + 2 * delta,
        }),
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let cycle = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(cycle["disk_observation"]["triggered"], true);
    assert!(!path(&e, &id).exists());
    assert!(path(&e, &other).exists());
    assert!(e.db.artifacts(&id).unwrap()[0].path.exists());
    assert!(e.db.artifacts(&other).unwrap()[0].path.exists());
    assert!(matches!(
        cycle["state"].as_str(),
        Some("complete" | "failed")
    ));
    if cycle["state"] == "failed" {
        assert!(
            cycle["error"]
                .as_str()
                .unwrap()
                .contains("disk-space target unmet")
        );
    }
    eprintln!(
        "real_disk_pressure_evidence {}",
        json!({"run_id":id,"foreign_run":other,"configured_thresholds":e.current_config().storage_policies[0].disk_pressure,"observation":cycle["disk_observation"],"cycle_state":cycle["state"],"scope_reclaimed":true,"other_scope_retained":true,"artifacts_retained":true,"disk_filled_by_test":false})
    );
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn unmet_pressure_carries_hysteresis_into_next_cycle_after_pin_release() {
    use preflight::storage_policy as policy;
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    let pinned = run(&e, &project.id, "retry-pressure").await;
    let disposable = run(&e, &project.id, "retry-pressure").await;
    let pin = pin_request(&project.id, &pinned);
    e.db.artifact_pin_set(&pin).unwrap();
    for id in [&pinned, &disposable] {
        sql(&e)
            .execute(
                "UPDATE runs SET finished_at=finished_at-120000 WHERE id=?1",
                [id],
            )
            .unwrap();
    }
    let mut config = e.config.clone();
    config.storage_policies = vec![policy::Policy {
        name: "retry-pressure".into(),
        project_id: project.id.clone(),
        pipeline_id: Some("retry-pressure".into()),
        category: policy::Category::ArtifactExpiry,
        ttl_seconds: 60,
        interval_seconds: 60,
        batch_limit: 2,
        quota: Some(policy::Quota {
            high_bytes: 15,
            low_bytes: 0,
        }),
        disk_pressure: None,
    }];
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config.clone()).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let first = &status["policies"][0]["recent_cycles"][0];
    assert_eq!(first["state"], "failed");
    assert_eq!(first["quota_observation"]["observed_file_bytes"], 8);
    assert!(!e.db.artifacts(&disposable).unwrap()[0].path.exists());
    assert!(e.db.artifacts(&pinned).unwrap()[0].path.exists());
    let mut release = pin_request(&project.id, &pinned);
    release.pinned = false;
    release.expected_revision = 1;
    release.request_id = "release-retry-pressure".into();
    e.db.artifact_pin_set(&release).unwrap();
    sql(&e)
        .execute(
            "UPDATE storage_policy_cycles SET updated_at=updated_at-61000 WHERE id=?1",
            [first["cycle_id"].as_str().unwrap()],
        )
        .unwrap();
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config).unwrap();
    policy::tick(&e).await.unwrap();
    let status = policy::status(&e).unwrap();
    let last = &status["policies"][0]["recent_cycles"][0];
    assert_ne!(last["cycle_id"], first["cycle_id"]);
    assert_eq!(last["state"], "complete");
    assert_eq!(last["quota_observation"]["observed_file_bytes"], 0);
    assert!(!e.db.artifacts(&pinned).unwrap()[0].path.exists());
    e.shutdown().await.unwrap();
    drop(e);
    drop(temp);
}
