#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{engine::Engine, evidence_expiry as expiry, storage_gc as gc};
use serde_json::{Value, json};
use std::sync::Arc;
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn sql(e: &Engine) -> rusqlite::Connection {
    rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3")).unwrap()
}
async fn run(e: &Arc<Engine>, project: &str, scope: &str, fail: bool) -> String {
    let run=e.start_pipeline(serde_json::from_value(json!({"project_id":project,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"sh","args":["-c",if fail{"echo 'error: retained failure' >&2; exit 1"}else{"echo 'warning: old success' >&2; printf result > result.txt"}],"timeout_seconds":5},"outputs":if fail{json!({})}else{json!({"result":{"path":"result.txt","type":"file"}})}}]}}})).unwrap()).unwrap();
    common::finished(e, &run.id).await;
    sql(e).execute("UPDATE stage_runs SET definition_json=json_set(definition_json,'$.cache',json_object('pipeline_id',?2,'deterministic',json('true'),'inputs_complete',json('true'))) WHERE run_id=?1",rusqlite::params![run.id,scope]).unwrap();
    run.id
}
async fn preview(e: &Arc<Engine>, p: &str, scope: &str, category: &str) -> Value {
    expiry::preview(e.clone(),serde_json::from_value(json!({"project_id":p,"pipeline_id":scope,"category":category,"acknowledge_loss":true,"finished_before":now()})).unwrap()).await.unwrap()
}
fn request(p: &str, plan: &Value) -> gc::Execute {
    serde_json::from_value(json!({"project_id":p,"plan_id":plan["plan_id"],"request_id":format!("expire-{}",plan["plan_id"].as_str().unwrap()),"actor":"test","reason":"explicit owned fixture expiry"})).unwrap()
}
#[tokio::test]
async fn log_expiry_is_atomic_scoped_archived_and_preserves_ids_not_false_empty_success() {
    let f = common::Fixture::new();
    let a = run(&f.engine, &f.project.id, "one", false).await;
    let b = run(&f.engine, &f.project.id, "two", false).await;
    let failed = run(&f.engine, &f.project.id, "one", true).await;
    let page = f.engine.db.stage_logs(&a, None, 0).unwrap();
    let id = page.events[0].id;
    f.engine
        .db
        .logs_archive(
            &serde_json::from_value(json!({"project_id":f.project.id,"run_id":a})).unwrap(),
        )
        .unwrap();
    preflight::diagnostics::report(
        &f.engine,
        &serde_json::from_value(json!({"project_id":f.project.id,"run_id":a})).unwrap(),
    )
    .unwrap();
    let plan = preview(&f.engine, &f.project.id, "one", "logs").await;
    assert!(
        plan["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["run_id"] == failed && v["eligible"] == false)
    );
    let req = request(&f.project.id, &plan);
    let result = gc::execute(f.engine.clone(), req.clone()).await.unwrap();
    assert!(result["expired_log_payload_bytes"].as_u64().unwrap() > 0);
    assert_eq!(result["deleted_file_bytes"], 0);
    assert_eq!(gc::execute(f.engine.clone(), req).await.unwrap(), result);
    assert!(
        f.engine
            .db
            .stage_logs(&a, None, 0)
            .unwrap_err()
            .to_string()
            .contains("logs_expired")
    );
    assert!(
        !f.engine
            .db
            .stage_logs(&b, None, 0)
            .unwrap()
            .events
            .is_empty()
    );
    assert!(
        !f.engine
            .db
            .stage_logs(&failed, None, 0)
            .unwrap()
            .events
            .is_empty()
    );
    assert_eq!(f.engine.db.run(&a).unwrap().expired_payloads, vec!["logs"]);
    let c = sql(&f.engine);
    assert_eq!(
        c.query_row("SELECT count(*) FROM log_events WHERE id=?1", [id], |r| r
            .get::<_, u64>(
            0
        ))
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM diagnostics WHERE run_id=?1",
            [&a],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        preflight::evidence::read(
            &f.engine,
            &serde_json::from_value(
                json!({"project_id":f.project.id,"evidence_id":format!("log:{id}")})
            )
            .unwrap()
        )
        .unwrap_err()
        .to_string()
        .contains("logs_expired")
    );
    assert!(
        preflight::diagnostics::report(
            &f.engine,
            &serde_json::from_value(json!({"project_id":f.project.id,"run_id":a})).unwrap()
        )
        .is_err()
    );
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn source_expiry_rechecks_pins_and_leases_and_keeps_original_artifacts_history() {
    let f = common::Fixture::new();
    let a = run(&f.engine, &f.project.id, "one", false).await;
    let b = run(&f.engine, &f.project.id, "two", false).await;
    let snapshot = f.engine.db.run(&a).unwrap().workspace.join("source");
    assert!(snapshot.exists());
    let original = std::fs::read(f.project.root.join("helper")).unwrap();
    let plan = preview(&f.engine, &f.project.id, "one", "source_snapshots").await;
    sql(&f.engine)
        .execute(
            "INSERT INTO operation_leases VALUES ('hold',?1,'test','active',0,NULL)",
            [&a],
        )
        .unwrap();
    let protected = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(protected["items"][0]["state"], "skipped");
    assert!(snapshot.exists());
    sql(&f.engine)
        .execute(
            "UPDATE operation_leases SET state='released' WHERE id='hold'",
            [],
        )
        .unwrap();
    let plan = preview(&f.engine, &f.project.id, "one", "source_snapshots").await;
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert!(result["deleted_file_bytes"].as_u64().unwrap() > 0);
    assert!(!snapshot.exists());
    assert_eq!(
        f.engine.db.run(&a).unwrap().expired_payloads,
        vec!["source_snapshots"]
    );
    assert!(
        f.engine
            .db
            .run(&b)
            .unwrap()
            .workspace
            .join("source")
            .exists()
    );
    assert_eq!(
        std::fs::read(f.project.root.join("helper")).unwrap(),
        original
    );
    assert!(std::path::Path::new(&f.engine.db.artifacts(&a).unwrap()[0].path).exists());
    assert!(f.engine.db.inputs(&a).unwrap().is_some());
    assert!(
        !f.engine
            .db
            .stage_logs(&a, None, 0)
            .unwrap()
            .events
            .is_empty()
    );
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn source_expiry_replays_intent_before_rename_after_restart() {
    let common::Fixture {
        temp,
        engine,
        project,
    } = common::Fixture::new();
    let id = run(&engine, &project.id, "one", false).await;
    let plan = preview(&engine, &project.id, "one", "source_snapshots").await;
    let req = request(&project.id, &plan);
    let c = sql(&engine);
    c.execute(
        "INSERT INTO evidence_expiry VALUES (?1,'source_snapshots',?2,0,?3)",
        rusqlite::params![id, req.plan_id, now()],
    )
    .unwrap();
    c.execute(
        "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
        [&req.plan_id],
    )
    .unwrap();
    drop(c);
    let config = engine.config.clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let e = Engine::open(config).unwrap();
    let result = gc::execute(e.clone(), req).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert!(!e.db.run(&id).unwrap().workspace.join("source").exists());
    e.shutdown().await.unwrap();
    drop(temp);
}
#[tokio::test]
async fn changed_logs_and_foreign_plan_never_delete_payload() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one", false).await;
    let plan = preview(&f.engine, &f.project.id, "one", "logs").await;
    sql(&f.engine)
        .execute(
            "UPDATE log_events SET text=text||'changed' WHERE run_id=?1",
            [&id],
        )
        .unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "failed");
    assert!(
        !f.engine
            .db
            .stage_logs(&id, None, 0)
            .unwrap()
            .events
            .is_empty()
    );
    let mut req = request(&f.project.id, &plan);
    req.project_id = uuid::Uuid::now_v7().to_string();
    assert!(gc::execute(f.engine.clone(), req).await.is_err());
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn disk_floor_rejects_new_preparation_without_stopping_existing_engine() {
    let mut f = common::Fixture::new();
    let mut config = f.engine.config.clone();
    config.min_free_disk_bytes = Some(9_007_199_254_740_991);
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    f.engine = Engine::open(config).unwrap();
    let id = run(&f.engine, &f.project.id, "one", false).await;
    let run = f.engine.db.run(&id).unwrap();
    assert_eq!(run.status, preflight::model::RunState::InfrastructureError);
    assert!(run.reason.unwrap().contains("disk_admission_denied"));
    assert!(
        f.engine
            .db
            .stage_logs(&id, None, 0)
            .unwrap()
            .events
            .is_empty()
    );
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn cache_ranking_and_eviction_invalidates_only_approved_unowned_outputs() {
    let f = common::Fixture::new();
    let a = run(&f.engine, &f.project.id, "one", false).await;
    let b = run(&f.engine, &f.project.id, "one", false).await;
    let c = sql(&f.engine);
    for (entry, run, hits, cost) in [("cold", &a, 0, 1), ("valuable", &b, 20, 10000)] {
        c.execute(
            "INSERT INTO stage_cache VALUES (?1,?2,'one',?3,?4,'build','active')",
            rusqlite::params![entry, f.project.id, format!("key-{entry}"), run],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cache_access VALUES (?1,?2,?3,?4)",
            rusqlite::params![entry, hits, now() - 10000, cost],
        )
        .unwrap();
    }
    let plan=preflight::cache_eviction::preview(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"pipeline_id":"one","finished_before":now(),"acknowledge_loss":true,"limit":1})).unwrap()).await.unwrap();
    assert_eq!(plan["entries"][0]["entry_id"], "cold");
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert_eq!(
        c.query_row("SELECT state FROM stage_cache WHERE id='cold'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "invalidated"
    );
    assert_eq!(
        c.query_row(
            "SELECT state FROM stage_cache WHERE id='valuable'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "active"
    );
    assert!(!std::path::Path::new(&f.engine.db.artifacts(&a).unwrap()[0].path).exists());
    assert!(std::path::Path::new(&f.engine.db.artifacts(&b).unwrap()[0].path).exists());
    assert!(
        f.engine
            .db
            .run(&a)
            .unwrap()
            .workspace
            .join("source")
            .exists()
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn automatic_log_source_and_cache_policies_page_to_targets_without_skipping_evicted_rows() {
    let common::Fixture {
        temp,
        engine,
        project,
    } = common::Fixture::new();
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(run(&engine, &project.id, "one", false).await);
        let id = &ids[n];
        let c = sql(&engine);
        c.execute(
            "UPDATE runs SET finished_at=?2 WHERE id=?1",
            rusqlite::params![id, now() - 120000],
        )
        .unwrap();
        c.execute(
            "INSERT INTO stage_cache VALUES (?1,?2,'one',?1,?3,'build','active')",
            rusqlite::params![format!("cache-{n}"), project.id, id],
        )
        .unwrap();
    }
    let mut config = engine.config.clone();
    config.storage_policies=serde_json::from_value(json!([
 {"name":"cache","project_id":project.id,"pipeline_id":"one","category":"cache_artifacts","ttl_seconds":60,"interval_seconds":60,"batch_limit":1,"quota":{"high_bytes":1,"low_bytes":0}},
 {"name":"sources","project_id":project.id,"pipeline_id":"one","category":"source_snapshots","ttl_seconds":60,"interval_seconds":60,"batch_limit":1},
 {"name":"logs","project_id":project.id,"pipeline_id":"one","category":"logs","ttl_seconds":60,"interval_seconds":60,"batch_limit":1,"quota":{"high_bytes":1,"low_bytes":0}}
 ])).unwrap();
    engine.shutdown().await.unwrap();
    drop(engine);
    let e = Engine::open(config).unwrap();
    for _ in 0..8 {
        preflight::storage_policy::tick(&e).await.unwrap();
    }
    // Simulate the next policy interval after cache owners have been evicted.
    sql(&e)
        .execute("UPDATE storage_policy_cycles SET updated_at=0", [])
        .unwrap();
    for _ in 0..8 {
        preflight::storage_policy::tick(&e).await.unwrap();
    }
    let c = sql(&e);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM stage_cache WHERE state='active'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM evidence_expiry", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        6
    );
    let status = preflight::storage_policy::status(&e).unwrap();
    for p in status["policies"].as_array().unwrap() {
        assert_eq!(p["recent_cycles"][0]["state"], "complete", "{p}");
    }
    e.shutdown().await.unwrap();
    drop(temp);
}

#[tokio::test]
async fn module_ttl_uses_actual_publication_time_and_rechecks_republication() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one", false).await;
    let publish: preflight::modules::Publish =
        serde_json::from_value(json!({"run_id":id,"stage_id":"build","artifact_name":"result"}))
            .unwrap();
    let artifact = preflight::modules::publish(&f.engine, &publish).unwrap();
    let c = sql(&f.engine);
    let request = || {
        serde_json::from_value(json!({"project_id":f.project.id,"finished_before":now()-60000}))
            .unwrap()
    };
    let recent = preflight::module_gc::preview(f.engine.clone(), request())
        .await
        .unwrap();
    assert!(recent["entries"].as_array().unwrap().is_empty());
    c.execute(
        "UPDATE module_publications SET published_at=?1",
        [now() - 120000],
    )
    .unwrap();
    let plan = preflight::module_gc::preview(f.engine.clone(), request())
        .await
        .unwrap();
    assert_eq!(plan["entries"][0]["eligible"], true);
    preflight::modules::publish(&f.engine, &publish).unwrap();
    let result = gc::execute(f.engine.clone(), self::request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "skipped");
    assert!(
        std::path::Path::new(
            &f.engine
                .db
                .module(&f.project.id, &artifact.sha256)
                .unwrap()
                .path
        )
        .exists()
    );
    c.execute(
        "UPDATE module_publications SET published_at=?1",
        [now() - 120000],
    )
    .unwrap();
    let copy = f
        .engine
        .db
        .module(&f.project.id, &artifact.sha256)
        .unwrap()
        .path;
    let mut config = f.engine.config.clone();
    config.storage_policies=serde_json::from_value(json!([{"name":"modules","project_id":f.project.id,"pipeline_id":"one","category":"module_copies","ttl_seconds":60,"interval_seconds":60,"batch_limit":1}])).unwrap();
    drop(c);
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let e = Engine::open(config).unwrap();
    preflight::storage_policy::tick(&e).await.unwrap();
    assert!(!std::path::Path::new(&copy).exists());
    assert!(std::path::Path::new(&e.db.artifacts(&id).unwrap()[0].path).exists());
    e.shutdown().await.unwrap();
}

#[tokio::test]
async fn retention_tools_http_and_mcp_share_frozen_plan_and_result() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one", false).await;
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
    let args = json!({"project_id":f.project.id,"run_id":id,"category":"logs","acknowledge_loss":true,"finished_before":now()});
    let output = client
        .call_tool(
            CallToolRequestParams::new("evidence_expiry_preview")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(output.is_error, Some(true));
    let plan: Value = serde_json::from_str(&output.content[0].as_text().unwrap().text).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let other: Value = http
        .post(format!("http://{addr}/api/v1/storage/evidence/preview"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["entries"], other["entries"]);
    let req = serde_json::to_value(request(&f.project.id, &plan)).unwrap();
    let output = client
        .call_tool(
            CallToolRequestParams::new("storage_gc_execute")
                .with_arguments(req.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(output.is_error, Some(true));
    let result: Value = serde_json::from_str(&output.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    let status: Value = http
        .get(format!(
            "http://{addr}/api/v1/storage-gc-status?project_id={}&plan_id={}",
            f.project.id,
            plan["plan_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status, result);
    let output = client
        .call_tool(
            CallToolRequestParams::new("cache_eviction_preview").with_arguments(
                json!({"project_id":f.project.id,"acknowledge_loss":true,"finished_before":now()})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(output.is_error, Some(true));
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires an explicitly owned disposable 64 MiB filesystem via PREFLIGHT_TEST_VOLUME"]
async fn full_disposable_volume_keeps_gc_fail_closed_and_recovers_after_space_returns() {
    use std::io::Write;
    let volume = std::path::PathBuf::from(
        std::env::var("PREFLIGHT_TEST_VOLUME").expect("explicit disposable volume required"),
    );
    let capacity = fs2::total_space(&volume).unwrap();
    assert!(
        capacity <= 128 * 1024 * 1024,
        "refuse to fill an ordinary filesystem"
    );
    let owned = tempfile::tempdir_in(&volume).unwrap();
    let f = common::Fixture::new();
    let mut config = f.engine.config.clone();
    config.data_dir = owned.path().join("data");
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let e = Engine::open(config).unwrap();
    let p = e
        .register(preflight::model::RegisterProject {
            name: "owned-volume".into(),
            path: f.project.root.to_string_lossy().into(),
        })
        .unwrap();
    let id = run(&e, &p.id, "one", false).await;
    let plan = gc::preview(
        e.clone(),
        serde_json::from_value(json!({"project_id":p.id,"pipeline_id":"one"})).unwrap(),
    )
    .await
    .unwrap();
    let req = request(&p.id, &plan);
    let original = std::fs::read(f.project.root.join("helper")).unwrap();
    let filler = owned.path().join("owned-filler");
    let mut file = std::fs::File::create(&filler).unwrap();
    let block = [0x5a; 65536];
    let mut full = false;
    for _ in 0..2048 {
        match file.write_all(&block) {
            Ok(()) => {}
            Err(error) => {
                assert!(
                    error.raw_os_error() == Some(28)
                        || error.kind() == std::io::ErrorKind::StorageFull,
                    "unexpected volume error: {error}"
                );
                full = true;
                break;
            }
        }
    }
    drop(file);
    assert!(full, "disposable volume did not reach ENOSPC");
    let outcome = gc::execute(e.clone(), req.clone()).await;
    let outcome_kind = if outcome.is_ok() {
        "reported_result"
    } else {
        "reported_error"
    };
    std::fs::remove_file(&filler).unwrap();
    // Recover moving/quarantined work through the identical audited request.
    let recovered = gc::execute(e.clone(), req).await.unwrap();
    assert_eq!(recovered["complete"], true);
    let c = sql(&e);
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        std::fs::read(f.project.root.join("helper")).unwrap(),
        original
    );
    assert!(e.db.run(&id).unwrap().workspace.join("source").exists());
    assert!(std::path::Path::new(&e.db.artifacts(&id).unwrap()[0].path).exists());
    assert!(!e.db.stage_logs(&id, None, 0).unwrap().events.is_empty());
    println!(
        "full_volume_evidence {}",
        json!({"capacity_bytes":capacity,"enospace_observed":true,"gc_at_full":outcome_kind,"after_recovery":recovered,"metadata_integrity":"ok","original_sources_and_artifacts_preserved":true})
    );
    e.shutdown().await.unwrap();
}

#[tokio::test]
async fn cache_eviction_replays_its_committed_invalidation_before_rename() {
    let common::Fixture {
        temp,
        engine,
        project,
    } = common::Fixture::new();
    let id = run(&engine, &project.id, "one", false).await;
    let c = sql(&engine);
    c.execute(
        "INSERT INTO stage_cache VALUES ('owned-cache',?1,'one','key',?2,'build','active')",
        rusqlite::params![project.id, id],
    )
    .unwrap();
    let plan = preflight::cache_eviction::preview(
        engine.clone(),
        serde_json::from_value(
            json!({"project_id":project.id,"acknowledge_loss":true,"finished_before":now()}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let req = request(&project.id, &plan);
    c.execute(
        "INSERT INTO artifact_expiry VALUES (?1,'build',?2,0)",
        rusqlite::params![id, req.plan_id],
    )
    .unwrap();
    c.execute(
        "UPDATE stage_cache SET state='invalidated' WHERE id='owned-cache'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE storage_gc_items SET state='moving' WHERE plan_id=?1",
        [&req.plan_id],
    )
    .unwrap();
    drop(c);
    let config = engine.config.clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let e = Engine::open(config).unwrap();
    let result = gc::execute(e.clone(), req).await.unwrap();
    assert_eq!(result["items"][0]["state"], "deleted");
    assert!(!std::path::Path::new(&e.db.artifacts(&id).unwrap()[0].path).exists());
    assert!(e.db.run(&id).unwrap().workspace.join("source").exists());
    e.shutdown().await.unwrap();
    drop(temp);
}

#[tokio::test]
async fn injected_mid_log_expiry_failure_rolls_back_tombstone_payload_and_index() {
    let f = common::Fixture::new();
    let id = run(&f.engine, &f.project.id, "one", false).await;
    preflight::diagnostics::report(
        &f.engine,
        &serde_json::from_value(json!({"project_id":f.project.id,"run_id":id})).unwrap(),
    )
    .unwrap();
    let before = f.engine.db.stage_logs(&id, None, 0).unwrap();
    let plan = preview(&f.engine, &f.project.id, "one", "logs").await;
    let c = sql(&f.engine);
    c.execute_batch("CREATE TRIGGER test_reject_log_expiry BEFORE UPDATE OF text ON log_events BEGIN SELECT RAISE(ABORT,'owned test fault'); END;").unwrap();
    let result = gc::execute(f.engine.clone(), request(&f.project.id, &plan))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["state"], "failed");
    assert!(f.engine.db.run(&id).unwrap().expired_payloads.is_empty());
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(f.engine.db.stage_logs(&id, None, 0).unwrap()).unwrap()
    );
    assert!(
        c.query_row(
            "SELECT count(*) FROM diagnostics WHERE run_id=?1",
            [&id],
            |r| r.get::<_, u64>(0)
        )
        .unwrap()
            > 0
    );
    assert!(
        c.query_row(
            "SELECT count(*) FROM log_search f JOIN log_events l ON l.id=f.rowid WHERE l.run_id=?1",
            [&id],
            |r| r.get::<_, u64>(0)
        )
        .unwrap()
            > 0
    );
    c.execute_batch("DROP TRIGGER test_reject_log_expiry")
        .unwrap();
    f.engine.shutdown().await.unwrap();
}
