#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{archives as a, control, management, model::*};
use serde_json::{Value, json};
use std::{io::Read, path::Path};
async fn close_owned_engine(engine: std::sync::Arc<preflight::engine::Engine>) {
    engine.shutdown().await.unwrap();
    let owner = std::sync::Arc::downgrade(&engine);
    drop(engine);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while owner.strong_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn save(f: &common::Fixture, slow: bool) {
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":"archive-build","expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"sh","args":["-c",if slow{"sleep 30"}else{"printf 'archive log ✓'; printf retained > result.txt; mkdir empty"}]},"outputs":{"result":{"path":"result.txt","type":"file"}}}]}}})).unwrap()).unwrap();
}
fn start(f: &common::Fixture) -> Run {
    control::start_saved(
        &f.engine,
        control::ConfigRef {
            project_id: f.project.id.clone(),
            name: "archive-build".into(),
            revision: Some(1),
        },
    )
    .unwrap()
}
fn create(f: &common::Fixture, run: &str) -> a::Create {
    serde_json::from_value(json!({"project_id":f.project.id,"run_id":run,"request_id":"test-archive","environment":true,"actor":"user","reason":"retain exact test run"})).unwrap()
}
fn id(f: &common::Fixture, archive: &Value) -> a::Id {
    a::Id {
        project_id: f.project.id.clone(),
        archive_id: archive["archive_id"].as_str().unwrap().into(),
    }
}
async fn ready(f: &common::Fixture, archive: &Value) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let v = a::get(&f.engine, &id(f, archive)).unwrap();
            if v["state"] == "ready" || v["state"] == "failed" {
                return v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn portable_snapshot_survives_physical_cleanup_and_restores_isolated_files() {
    let f = common::Fixture::new();
    std::fs::write(f.project.root.join("source.txt"), "source content").unwrap();
    save(&f, false);
    let run = start(&f);
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    f.engine
        .db
        .logs_archive(&preflight::log_archive::Request {
            project_id: f.project.id.clone(),
            run_id: run.id.clone(),
            max_chunks: 4,
        })
        .unwrap();
    let archive = a::create(&f.engine, create(&f, &run.id)).unwrap();
    assert_eq!(
        a::create(&f.engine, create(&f, &run.id)).unwrap()["archive_id"],
        archive["archive_id"]
    );
    let state = ready(&f, &archive).await;
    assert_eq!(state["state"], "ready", "{state}");
    assert!(state["summary"]["missing"].as_array().unwrap().is_empty());
    assert_eq!(
        a::verify(f.engine.clone(), id(&f, &archive)).await.unwrap()["verified"],
        true
    );
    let file = f.engine.config.data_dir.join("archives").join(format!(
        "{}.tar.zst",
        archive["archive_id"].as_str().unwrap()
    ));
    let decoder = zstd::stream::read::Decoder::new(std::fs::File::open(&file).unwrap()).unwrap();
    let mut tar = tar::Archive::new(decoder);
    let mut logs = String::new();
    for item in tar.entries().unwrap() {
        let mut item = item.unwrap();
        if item.path().unwrap().as_ref() == Path::new("logs.jsonl") {
            item.read_to_string(&mut logs).unwrap();
        }
    }
    assert!(logs.contains("archive log ✓"));
    let parsed: Value = serde_json::from_str(logs.lines().next().unwrap()).unwrap();
    assert!(parsed["id"].is_number());
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let plan=management::preview(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"action":"artifacts","pipeline_key":"saved:archive-build"})).unwrap()).await.unwrap();
    preflight::storage_gc::execute(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"plan_id":plan["plan_id"],"request_id":"cleanup","actor":"user","reason":"test original cleanup after backup"})).unwrap()).await.unwrap();
    assert!(!f.engine.db.artifacts(&run.id).unwrap()[0].path.exists());
    let restored=a::restore(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"archive_id":archive["archive_id"],"request_id":"restore-once","actor":"user","reason":"recover snapshot"})).unwrap()).await.unwrap();
    let path = Path::new(restored["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("workspace/artifacts/build/result")).unwrap(),
        "retained"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("workspace/source/source.txt")).unwrap(),
        "source content"
    );
    assert!(path.join("workspace/stages/build/empty").is_dir());
    assert!(a::restore(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"archive_id":archive["archive_id"],"request_id":"restore-once","actor":"user","reason":"recover snapshot"})).unwrap()).await.is_err());
    assert_eq!(
        f.engine.db.run(&run.id).unwrap().status,
        RunState::Succeeded
    );
    let mut changed = create(&f, &run.id);
    changed.source = false;
    assert!(a::create(&f.engine, changed).is_err());
}
#[tokio::test]
async fn archive_rejects_active_runs_links_foreign_ownership_and_corruption() {
    let f = common::Fixture::new();
    save(&f, true);
    let run = start(&f);
    assert!(a::create(&f.engine, create(&f, &run.id)).is_err());
    f.engine
        .cancel(CancelRun {
            run_id: run.id.clone(),
            actor: Actor::User,
            reason: "test".into(),
        })
        .unwrap();
    common::finished(&f.engine, &run.id).await;
    std::fs::create_dir_all(run.workspace.join("stages")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", run.workspace.join("stages/escape")).unwrap();
    let archive = a::create(&f.engine, create(&f, &run.id)).unwrap();
    assert_eq!(ready(&f, &archive).await["state"], "failed");
    std::fs::remove_file(run.workspace.join("stages/escape")).unwrap();
    let mut request = create(&f, &run.id);
    request.request_id = "clean".into();
    let archive = a::create(&f.engine, request).unwrap();
    assert_eq!(ready(&f, &archive).await["state"], "ready");
    assert!(
        a::get(
            &f.engine,
            &a::Id {
                project_id: "foreign".into(),
                archive_id: archive["archive_id"].as_str().unwrap().into()
            }
        )
        .is_err()
    );
    let path = f.engine.config.data_dir.join("archives").join(format!(
        "{}.tar.zst",
        archive["archive_id"].as_str().unwrap()
    ));
    let mut data = std::fs::read(&path).unwrap();
    data[10] ^= 1;
    std::fs::write(&path, data).unwrap();
    assert!(a::verify(f.engine.clone(), id(&f, &archive)).await.is_err());
    assert!(a::restore(f.engine.clone(),serde_json::from_value(json!({"project_id":f.project.id,"archive_id":archive["archive_id"],"request_id":"bad","actor":"user","reason":"reject bad archive"})).unwrap()).await.is_err());
}
#[tokio::test]
async fn mcp_archive_tools_and_http_download_share_verified_identity() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    save(&f, false);
    let run = start(&f);
    common::finished(&f.engine, &run.id).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = f.engine.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, preflight::api::router(engine))
            .await
            .unwrap();
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{addr}/mcp"
        )))
        .await
        .unwrap();
    let result = client
        .call_tool(
            CallToolRequestParams::new("archives_create").with_arguments(
                serde_json::to_value(create(&f, &run.id))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let archive = a::list(
        &f.engine,
        serde_json::from_value(json!({"project_id":f.project.id})).unwrap(),
    )
    .unwrap()["items"][0]
        .clone();
    assert_eq!(ready(&f, &archive).await["state"], "ready");
    let args = json!({"project_id":f.project.id,"archive_id":archive["archive_id"]});
    for name in ["archives_get", "archives_verify"] {
        let response = client
            .call_tool(
                CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true));
    }
    for (name, args) in [
        (
            "archives_pin",
            json!({"project_id":f.project.id,"archive_id":archive["archive_id"],"pinned":true,"expected_revision":0,"request_id":"mcp-pin","actor":"agent","reason":"MCP retention parity"}),
        ),
        (
            "archives_cleanup_preview",
            json!({"project_id":f.project.id,"finished_before":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64}),
        ),
    ] {
        let response = client
            .call_tool(
                CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{name}: {response:?}");
    }
    let listed = client
        .call_tool(
            CallToolRequestParams::new("archives_list").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(listed.is_error, Some(true));
    let restored=client.call_tool(CallToolRequestParams::new("archives_restore").with_arguments(json!({"project_id":f.project.id,"archive_id":archive["archive_id"],"request_id":"mcp-restore","actor":"agent","reason":"verify isolated recovery through MCP"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(restored.is_error, Some(true));
    let download = reqwest::get(format!(
        "http://{addr}/api/v1/archive/download?project_id={}&archive_id={}",
        f.project.id,
        archive["archive_id"].as_str().unwrap()
    ))
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    assert_eq!(download.headers()["content-type"], "application/zstd");
    assert_eq!(
        download.content_length(),
        a::get(&f.engine, &id(&f, &archive)).unwrap()["file_bytes"].as_u64()
    );
    assert!(
        download.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains(".tar.zst")
    );
    let bytes = download.bytes().await.unwrap();
    use sha2::Digest;
    assert_eq!(
        preflight::hashing::hex(sha2::Sha256::digest(&bytes)),
        a::get(&f.engine, &id(&f, &archive)).unwrap()["sha256"]
    );
    client.cancel().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn interrupted_creation_is_failed_on_restart_without_changing_run() {
    let f = common::Fixture::new();
    save(&f, false);
    let run = start(&f);
    common::finished(&f.engine, &run.id).await;
    let archive = uuid::Uuid::now_v7().to_string();
    {
        let c = preflight::db::open(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
        c.execute("INSERT INTO run_archives(id,project_id,run_id,request_id,request_json,state,created_at) VALUES (?1,?2,?3,'interrupted','{}','creating',0)",rusqlite::params![archive,f.project.id,run.id]).unwrap();
    }
    let common::Fixture {
        temp,
        engine,
        project,
    } = f;
    let config = engine.config.clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = preflight::engine::Engine::open(config).unwrap();
    let result = a::get(
        &reopened,
        &a::Id {
            project_id: project.id,
            archive_id: archive,
        },
    )
    .unwrap();
    assert_eq!(result["state"], "failed");
    assert!(result["error"].as_str().unwrap().contains("restarted"));
    assert_eq!(
        reopened.db.run(&run.id).unwrap().status,
        RunState::Succeeded
    );
    reopened.shutdown().await.unwrap();
    drop(reopened);
    drop(temp);
}

#[tokio::test]
async fn external_import_preserves_origin_without_forging_a_run_and_survives_restart() {
    let source = common::Fixture::new();
    save(&source, false);
    let run = start(&source);
    common::finished(&source.engine, &run.id).await;
    let archive = a::create(&source.engine, create(&source, &run.id)).unwrap();
    let state = ready(&source, &archive).await;
    assert_eq!(state["state"], "ready");
    let source_path = source
        .engine
        .config
        .data_dir
        .join("archives")
        .join(format!("{}.tar.zst", state["archive_id"].as_str().unwrap()));
    let target = common::Fixture::new();
    let inbox = a::inbox_location(
        &target.engine,
        control::ProjectId {
            project_id: target.project.id.clone(),
        },
    )
    .unwrap();
    let staged = Path::new(inbox["path"].as_str().unwrap()).join("external.tar.zst");
    std::fs::copy(source_path, &staged).unwrap();
    let request = || {
        serde_json::from_value::<a::ImportStaged>(json!({
        "project_id":target.project.id,"sha256":state["sha256"],"file_name":"external.tar.zst",
        "request_id":"import-once","actor":"user","reason":"recover external archive"}))
        .unwrap()
    };
    let imported = a::import_staged(target.engine.clone(), request())
        .await
        .unwrap();
    assert_eq!(imported["imported"], true);
    assert_eq!(imported["origin_project_id"], source.project.id);
    assert_eq!(imported["run_id"], run.id);
    assert!(target.engine.db.run(&run.id).is_err());
    assert_eq!(
        a::import_staged(target.engine.clone(), request())
            .await
            .unwrap()["archive_id"],
        imported["archive_id"]
    );
    let mut conflict = request();
    conflict.import.reason = "changed".into();
    assert!(
        a::import_staged(target.engine.clone(), conflict)
            .await
            .is_err()
    );
    let mut traversal = request();
    traversal.file_name = "../external.tar.zst".into();
    assert!(
        a::import_staged(target.engine.clone(), traversal)
            .await
            .is_err()
    );
    let listing = a::list(
        &target.engine,
        serde_json::from_value(json!({"project_id":target.project.id})).unwrap(),
    )
    .unwrap();
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        a::verify(target.engine.clone(), id(&target, &imported))
            .await
            .unwrap()["verified"],
        true
    );
    let config = target.engine.config.clone();
    target.engine.shutdown().await.unwrap();
    let project_id = target.project.id.clone();
    let archive_id = imported["archive_id"].as_str().unwrap().to_string();
    drop(target.engine);
    let reopened = preflight::engine::Engine::open(config).unwrap();
    let restored=a::restore(reopened.clone(),serde_json::from_value(json!({"project_id":project_id,"archive_id":archive_id,"request_id":"recover-import","actor":"user","reason":"restore imported files"})).unwrap()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(
            Path::new(restored["path"].as_str().unwrap()).join("workspace/artifacts/build/result")
        )
        .unwrap(),
        "retained"
    );
    assert!(reopened.db.run(&run.id).is_err());
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn external_import_http_and_mcp_reject_wrong_digest_and_links() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let source = common::Fixture::new();
    save(&source, false);
    let run = start(&source);
    common::finished(&source.engine, &run.id).await;
    let archive = a::create(&source.engine, create(&source, &run.id)).unwrap();
    let state = ready(&source, &archive).await;
    let path = source
        .engine
        .config
        .data_dir
        .join("archives")
        .join(format!("{}.tar.zst", state["archive_id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    let target = common::Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = target.engine.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, preflight::api::router(engine))
            .await
            .unwrap();
    });
    let sha = state["sha256"].as_str().unwrap();
    let url = |hash: &str| {
        format!(
            "http://{addr}/api/v1/archive/import?project_id={}&request_id=http-import&sha256={hash}&actor=user&reason=import",
            target.project.id
        )
    };
    let http = reqwest::Client::new();
    let bad = http
        .post(url(&"0".repeat(64)))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert!(
        a::list(
            &target.engine,
            serde_json::from_value(json!({"project_id":target.project.id})).unwrap()
        )
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let imported: Value = http
        .post(url(sha))
        .body(bytes)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(imported["imported"], true);
    let inbox = a::inbox_location(
        &target.engine,
        control::ProjectId {
            project_id: target.project.id.clone(),
        },
    )
    .unwrap();
    let root = Path::new(inbox["path"].as_str().unwrap());
    std::os::unix::fs::symlink(&path, root.join("linked.zst")).unwrap();
    let linked=serde_json::from_value(json!({"project_id":target.project.id,"file_name":"linked.zst","sha256":sha,"request_id":"link","actor":"user","reason":"reject link"})).unwrap();
    assert!(
        a::import_staged(target.engine.clone(), linked)
            .await
            .is_err()
    );
    std::fs::copy(path, root.join("valid.zst")).unwrap();
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{addr}/mcp"
        )))
        .await
        .unwrap();
    let response=client.call_tool(CallToolRequestParams::new("archives_import").with_arguments(json!({"project_id":target.project.id,"file_name":"valid.zst","sha256":sha,"request_id":"mcp-import","actor":"agent","reason":"MCP import"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    client.cancel().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn schema_49_upgrade_preserves_existing_archive_and_history() {
    let f = common::Fixture::new();
    save(&f, false);
    let run = start(&f);
    common::finished(&f.engine, &run.id).await;
    let archive = a::create(&f.engine, create(&f, &run.id)).unwrap();
    assert_eq!(ready(&f, &archive).await["state"], "ready");
    let config = f.engine.config.clone();
    let project = f.project.id.clone();
    close_owned_engine(f.engine).await;
    let db = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    common::remove_schema_53_for_historical_fixture(&db);
    db.execute_batch("DROP TABLE archive_encryption; DROP TABLE archive_disposals; DROP TABLE archive_gc_items; DROP TABLE archive_pin_requests; DROP TABLE archive_pins; DROP VIEW archive_inventory; DROP TABLE archive_imports; DELETE FROM migration_history WHERE version>=50; DELETE FROM schema_version WHERE version>=50;").unwrap();
    drop(db);
    let engine = preflight::engine::Engine::open(config).unwrap();
    let key = a::Id {
        project_id: project,
        archive_id: archive["archive_id"].as_str().unwrap().into(),
    };
    let state = a::get(&engine, &key).unwrap();
    assert_eq!(state["imported"], false);
    assert_eq!(state["state"], "ready");
    assert_eq!(
        a::verify(engine.clone(), key).await.unwrap()["verified"],
        true
    );
    assert_eq!(engine.db.run(&run.id).unwrap().status, RunState::Succeeded);
    engine.shutdown().await.unwrap();
}

async fn retention_archives(f: &common::Fixture, count: usize) -> (Run, Vec<Value>) {
    save(f, false);
    let run = start(f);
    common::finished(&f.engine, &run.id).await;
    let mut archives = Vec::new();
    for n in 0..count {
        let mut request = create(f, &run.id);
        request.request_id = format!("retention-{n}");
        let archive = a::create(&f.engine, request).unwrap();
        let value = ready(f, &archive).await;
        assert_eq!(value["state"], "ready");
        archives.push(value);
    }
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    (run, archives)
}
fn retention_preview(f: &common::Fixture) -> preflight::archive_gc::Preview {
    serde_json::from_value(
        json!({"project_id":f.project.id,"finished_before":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,"keep_latest":1}),
    )
    .unwrap()
}
fn cleanup_request(f: &common::Fixture, plan: &Value) -> preflight::storage_gc::Execute {
    serde_json::from_value(json!({"project_id":f.project.id,"plan_id":plan["plan_id"],"request_id":"archive-cleanup","actor":"user","reason":"test retention"})).unwrap()
}
fn archive_path(f: &common::Fixture, archive: &Value) -> std::path::PathBuf {
    f.engine.config.data_dir.join("archives").join(format!(
        "{}.tar.zst",
        archive["archive_id"].as_str().unwrap()
    ))
}
#[tokio::test]
async fn archive_retention_protects_pins_latest_readers_and_run_history() {
    use preflight::{archive_gc as g, storage_gc as gc};
    let f = common::Fixture::new();
    let (run, archives) = retention_archives(&f, 3).await;
    let pin = || {
        serde_json::from_value(json!({"project_id":f.project.id,"archive_id":archives[0]["archive_id"],"pinned":true,"expected_revision":0,"request_id":"pin-first","actor":"user","reason":"retain recovery copy"})).unwrap()
    };
    let first = g::pin(&f.engine, pin()).unwrap();
    assert_eq!(g::pin(&f.engine, pin()).unwrap(), first);
    let plan = g::preview(f.engine.clone(), retention_preview(&f))
        .await
        .unwrap();
    assert_eq!(
        plan["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["eligible"] == true)
            .count(),
        1
    );
    let reader = std::fs::File::open(archive_path(&f, &archives[1])).unwrap();
    fs2::FileExt::try_lock_shared(&reader).unwrap();
    let blocked = gc::execute(f.engine.clone(), cleanup_request(&f, &plan))
        .await
        .unwrap();
    assert_eq!(blocked["complete"], false);
    assert_eq!(blocked["items"][0]["state"], "blocked");
    assert!(archive_path(&f, &archives[1]).exists());
    drop(reader);
    let removed = gc::execute(f.engine.clone(), cleanup_request(&f, &plan))
        .await
        .unwrap();
    assert_eq!(removed["complete"], true, "{removed}");
    assert!(removed["deleted_file_bytes"].as_u64().unwrap() > 0);
    assert!(!archive_path(&f, &archives[1]).exists());
    assert_eq!(
        gc::execute(f.engine.clone(), cleanup_request(&f, &plan))
            .await
            .unwrap(),
        removed
    );
    let mut changed = cleanup_request(&f, &plan);
    changed.reason = "changed".into();
    assert!(gc::execute(f.engine.clone(), changed).await.is_err());
    assert_eq!(
        a::get(&f.engine, &id(&f, &archives[1])).unwrap()["state"],
        "expired"
    );
    assert_eq!(
        a::get(&f.engine, &id(&f, &archives[0])).unwrap()["pinned"],
        true
    );
    assert!(archive_path(&f, &archives[0]).exists());
    assert!(archive_path(&f, &archives[2]).exists());
    assert_eq!(
        f.engine.db.run(&run.id).unwrap().status,
        RunState::Succeeded
    );
    assert!(f.engine.db.artifacts(&run.id).unwrap()[0].path.exists());
}
#[tokio::test]
async fn archive_retention_rechecks_changed_files_pins_and_scope() {
    use preflight::{archive_gc as g, storage_gc as gc};
    let f = common::Fixture::new();
    let (_, archives) = retention_archives(&f, 3).await;
    let mut foreign = retention_preview(&f);
    foreign.pipeline_key = Some("saved:another-pipeline".into());
    assert!(
        g::preview(f.engine.clone(), foreign).await.unwrap()["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let plan = g::preview(f.engine.clone(), retention_preview(&f))
        .await
        .unwrap();
    let path = archive_path(&f, &archives[0]);
    let mut data = std::fs::read(&path).unwrap();
    data[10] ^= 1;
    std::fs::write(&path, &data).unwrap();
    g::pin(&f.engine,serde_json::from_value(json!({"project_id":f.project.id,"archive_id":archives[1]["archive_id"],"pinned":true,"expected_revision":0,"request_id":"pin-after-preview","actor":"user","reason":"protect approved candidate"})).unwrap()).unwrap();
    let result = gc::execute(f.engine.clone(), cleanup_request(&f, &plan))
        .await
        .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["deleted_file_bytes"], 0);
    assert!(
        result["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["state"] == "skipped")
    );
    assert!(path.exists());
    assert_eq!(std::fs::read(path).unwrap(), data);
}
#[tokio::test]
async fn interrupted_archive_quarantine_resumes_after_restart_without_deleting_latest() {
    use preflight::{archive_gc as g, storage_gc as gc};
    let f = common::Fixture::new();
    let (run, archives) = retention_archives(&f, 2).await;
    let plan = g::preview(f.engine.clone(), retention_preview(&f))
        .await
        .unwrap();
    let request = cleanup_request(&f, &plan);
    let archive = archives[0]["archive_id"].as_str().unwrap();
    let plan_id = plan["plan_id"].as_str().unwrap();
    let trash = f
        .engine
        .config
        .data_dir
        .join("archives/.trash")
        .join(plan_id);
    std::fs::create_dir_all(&trash).unwrap();
    {
        let c = preflight::db::open(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
        c.execute(
            "UPDATE storage_gc_plans SET request_id=?2,request_json=?3 WHERE id=?1",
            rusqlite::params![
                plan_id,
                request.request_id,
                serde_json::to_string(&request).unwrap()
            ],
        )
        .unwrap();
        c.execute(
            "INSERT INTO archive_disposals VALUES (?1,?2,0,0)",
            rusqlite::params![archive, plan_id],
        )
        .unwrap();
        c.execute(
            "UPDATE archive_gc_items SET state='deleting' WHERE plan_id=?1",
            [plan_id],
        )
        .unwrap();
    }
    std::fs::rename(
        archive_path(&f, &archives[0]),
        trash.join(format!("{archive}.tar.zst")),
    )
    .unwrap();
    let config = f.engine.config.clone();
    let project = f.project.id.clone();
    close_owned_engine(f.engine).await;
    let reopened = preflight::engine::Engine::open(config).unwrap();
    let latest_file = reopened.config.data_dir.join("archives").join(format!(
        "{}.tar.zst",
        archives[1]["archive_id"].as_str().unwrap()
    ));
    let latest_bytes = std::fs::read(&latest_file).unwrap();
    std::fs::remove_file(&latest_file).unwrap();
    let suspended = gc::execute(reopened.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(suspended["complete"], false);
    assert!(trash.join(format!("{archive}.tar.zst")).exists());
    std::fs::write(latest_file, latest_bytes).unwrap();
    let result = gc::execute(reopened.clone(), request).await.unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["items"][0]["state"], "deleted");
    assert!(std::fs::read_dir(&trash).unwrap().next().is_none());
    assert_eq!(
        reopened.db.run(&run.id).unwrap().status,
        RunState::Succeeded
    );
    assert_eq!(
        a::get(
            &reopened,
            &a::Id {
                project_id: project,
                archive_id: archives[1]["archive_id"].as_str().unwrap().into()
            }
        )
        .unwrap()["state"],
        "ready"
    );
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn configured_archive_quota_uses_shared_plans_and_keeps_latest_copy() {
    let f = common::Fixture::new();
    let (_, archives) = retention_archives(&f, 3).await;
    {
        let c = preflight::db::open(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
        c.execute("UPDATE run_archives SET finished_at=1", [])
            .unwrap();
    }
    let config = f.engine.config.clone();
    let project = f.project.id.clone();
    close_owned_engine(f.engine).await;
    let mut config = config;
    config.storage_policies=vec![serde_json::from_value(json!({"name":"archive-quota","project_id":project,"category":"run_archives","ttl_seconds":60,"interval_seconds":60,"batch_limit":20,"quota":{"high_bytes":1,"low_bytes":0}})).unwrap()];
    let engine = preflight::engine::Engine::open(config).unwrap();
    preflight::storage_policy::tick(&engine).await.unwrap();
    for archive in &archives[..2] {
        assert_eq!(
            a::get(
                &engine,
                &a::Id {
                    project_id: project.clone(),
                    archive_id: archive["archive_id"].as_str().unwrap().into()
                }
            )
            .unwrap()["state"],
            "expired"
        );
    }
    assert_eq!(
        a::get(
            &engine,
            &a::Id {
                project_id: project.clone(),
                archive_id: archives[2]["archive_id"].as_str().unwrap().into()
            }
        )
        .unwrap()["state"],
        "ready"
    );
    let status = preflight::storage_policy::status(&engine).unwrap();
    assert!(
        status["policies"][0]["recent_cycles"][0]["error"]
            .as_str()
            .unwrap()
            .contains("quota target unmet")
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_latest_archive_never_authorizes_removal_of_the_last_real_copy() {
    let f = common::Fixture::new();
    let (_, archives) = retention_archives(&f, 2).await;
    std::fs::remove_file(archive_path(&f, &archives[1])).unwrap();
    assert!(
        preflight::archive_gc::preview(f.engine.clone(), retention_preview(&f))
            .await
            .is_err()
    );
    assert!(archive_path(&f, &archives[0]).is_file());
}
