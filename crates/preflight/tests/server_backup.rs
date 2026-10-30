#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{archive_crypto as crypto, server_backup as b};
use serde_json::json;
use std::{fs, path::Path};
fn hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    preflight::hashing::hex(Sha256::digest(fs::read(path).unwrap()))
}
#[tokio::test]
async fn offline_snapshot_cas_configuration_encryption_and_isolated_restore() {
    let f = common::Fixture::new();
    let config = f.temp.path().join("config.toml");
    fs::write(&config, toml::to_string(&f.engine.config).unwrap()).unwrap();
    let archive = f.temp.path().join("backup.tar.zst");
    assert!(
        b::create(&config, &archive)
            .unwrap_err()
            .to_string()
            .contains("server is running")
    );
    assert!(!archive.exists());
    let data = f.engine.config.data_dir.clone();
    let root = f.project.root.clone();
    let project_id = f.project.id.clone();
    fs::create_dir_all(data.join("workspaces/empty-artifact")).unwrap();
    let blob = data.join("modules").join("fixture");
    fs::create_dir_all(blob.parent().unwrap()).unwrap();
    fs::write(&blob, b"retained CAS bytes").unwrap();
    let definition = json!({"version":1,"pipeline":{"stages":[{"id":"test","kind":"test","command":{"program":"sh","args":["-c","printf 'backup console marker\\n'; printf 'retained artifact' > result.bin; printf '<testsuite tests=\"1\"><testcase name=\"retained-test\"/></testsuite>' > junit.xml"]},"outputs":{"result":{"path":"result.bin","type":"file"}},"junit":["junit.xml"]}]}});
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":project_id,"name":"qa","expected_revision":0,"definition":definition})).unwrap()).unwrap();
    let run = preflight::control::start_saved(
        &f.engine,
        preflight::control::ConfigRef {
            project_id: project_id.clone(),
            name: "qa".into(),
            revision: Some(1),
        },
    )
    .unwrap();
    let run = common::finished(&f.engine, &run.id).await;
    assert_eq!(run.status, preflight::model::RunState::Succeeded);
    let artifact = f.engine.db.artifacts(&run.id).unwrap().remove(0);
    assert_eq!(fs::read(&artifact.path).unwrap(), b"retained artifact");
    f.engine.shutdown().await.unwrap();
    let weak = std::sync::Arc::downgrade(&f.engine);
    let common::Fixture {
        temp,
        engine,
        project: _,
    } = f;
    drop(engine);
    for _ in 0..100 {
        if weak.strong_count() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(weak.strong_count(), 0);
    let report = b::create(&config, &archive).unwrap();
    assert_eq!(report["sha256"], hash(&archive));
    assert!(b::create(&config, &archive).is_err());
    assert_eq!(
        b::verify(&archive, report["sha256"].as_str().unwrap()).unwrap()["verified"],
        true
    );
    let sealed = temp.path().join("backup.age");
    let key = temp.path().join("identity");
    let public = crypto::keygen(&key).unwrap();
    crypto::seal(&archive, &sealed, public["recipient"].as_str().unwrap()).unwrap();
    let decrypted = temp.path().join("decrypted.tar.zst");
    crypto::unseal(&sealed, &decrypted, &hash(&sealed), &key).unwrap();
    assert_eq!(hash(&archive), hash(&decrypted));
    let destination = temp.path().join("restored");
    assert!(b::restore(&archive, &"0".repeat(64), &destination).is_err());
    assert!(!destination.exists());
    let result = b::restore(&decrypted, &hash(&decrypted), &destination).unwrap();
    assert_eq!(result["activated"], false);
    assert_eq!(
        fs::read(destination.join("data/modules/fixture")).unwrap(),
        b"retained CAS bytes"
    );
    assert!(
        destination
            .join(format!(
                "project-configs/{project_id}/.preflight/pipelines/qa/1.json"
            ))
            .is_file()
    );
    assert!(destination.join("RESTORE_NOT_ACTIVATED").exists());
    assert!(destination.join("data/workspaces/empty-artifact").is_dir());
    assert!(b::restore(&archive, &hash(&archive), &destination).is_err());
    assert!(data.exists());
    assert!(root.exists());
    // Explicit operator activation at the original logical path, only on this owned fixture.
    let old = temp.path().join("original-data");
    fs::rename(&data, &old).unwrap();
    fs::rename(destination.join("data"), &data).unwrap();
    fs::rename(
        root.join(".preflight"),
        root.join(".preflight-before-recovery"),
    )
    .unwrap();
    fs::rename(
        destination.join(format!("project-configs/{project_id}/.preflight")),
        root.join(".preflight"),
    )
    .unwrap();
    let restored_config =
        preflight::config::Config::load(&destination.join("server-config.toml")).unwrap();
    let engine = preflight::engine::Engine::open(restored_config).unwrap();
    assert_eq!(engine.db.project(&project_id).unwrap().root, root);
    assert_eq!(
        fs::read(data.join("modules/fixture")).unwrap(),
        b"retained CAS bytes"
    );
    assert_eq!(
        engine
            .db
            .pipeline_config(&preflight::control::ConfigRef {
                project_id: project_id.clone(),
                name: "qa".into(),
                revision: Some(1)
            })
            .unwrap()["revision"],
        1
    );
    let retained = engine.db.run(&run.id).unwrap();
    assert_eq!(retained.status, preflight::model::RunState::Succeeded);
    assert_eq!(retained.stages[0].tests.as_ref().unwrap().counts.passed, 1);
    assert!(common::output(&engine, &run.id, "stdout").contains("backup console marker"));
    assert_eq!(
        fs::read(engine.db.artifacts(&run.id).unwrap()[0].path.clone()).unwrap(),
        b"retained artifact"
    );
    engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn unresolved_work_symlinks_and_corruption_are_refused() {
    let f = common::Fixture::new();
    let config = f.temp.path().join("config.toml");
    fs::write(&config, toml::to_string(&f.engine.config).unwrap()).unwrap();
    let data = f.engine.config.data_dir.clone();
    let temp = f.temp;
    let engine = f.engine;
    let id = f.project.id;
    engine.shutdown().await.unwrap();
    drop(engine);
    let db = rusqlite::Connection::open(data.join("preflight.sqlite3")).unwrap();
    db.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,workspace) VALUES ('pending',?1,'queued','{}',1,'/unused')",[id]).unwrap();
    let archive = temp.path().join("backup");
    assert!(
        b::create(&config, &archive)
            .unwrap_err()
            .to_string()
            .contains("unresolved runs")
    );
    db.execute("DELETE FROM runs WHERE id='pending'", [])
        .unwrap();
    drop(db);
    let link = data.join("bad-link");
    std::os::unix::fs::symlink(temp.path(), &link).unwrap();
    assert!(
        b::create(&config, &archive)
            .unwrap_err()
            .to_string()
            .contains("symbolic links")
    );
    fs::remove_file(link).unwrap();
    let report = b::create(&config, &archive).unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    bytes.truncate(bytes.len() / 2);
    fs::write(&archive, bytes).unwrap();
    let target = temp.path().join("restore");
    assert!(b::restore(&archive, report["sha256"].as_str().unwrap(), &target).is_err());
    assert!(!target.exists());
    assert!(b::restore(&archive, &hash(&archive), &target).is_err());
    assert!(!target.exists());
    let db = rusqlite::Connection::open(data.join("preflight.sqlite3")).unwrap();
    db.execute(
        "UPDATE migration_history SET sha256='bad' WHERE version=1",
        [],
    )
    .unwrap();
    drop(db);
    assert!(
        b::create(&config, &temp.path().join("bad-ledger"))
            .unwrap_err()
            .to_string()
            .contains("migration checksum mismatch")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn previous_schema_backup_restores_then_upgrades_without_rewriting_history() {
    let f = common::Fixture::new();
    let configuration = f.temp.path().join("config.toml");
    let mut cfg = f.engine.config.clone();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    cfg.bind = port.local_addr().unwrap();
    drop(port);
    fs::write(&configuration, toml::to_string(&cfg).unwrap()).unwrap();
    let data = f.engine.config.data_dir.clone();
    let project = f.project.id;
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let c = rusqlite::Connection::open(data.join("preflight.sqlite3")).unwrap();
    common::remove_schema_53_for_historical_fixture(&c);
    c.execute_batch("DROP TABLE archive_encryption; DELETE FROM schema_version WHERE version=52; DELETE FROM migration_history WHERE version=52;").unwrap();
    drop(c);
    let archive = f.temp.path().join("schema-51.tar.zst");
    let report = b::create(&configuration, &archive).unwrap();
    assert_eq!(
        b::verify(&archive, report["sha256"].as_str().unwrap()).unwrap()["schema"],
        51
    );
    let restored = f.temp.path().join("restore");
    b::restore(&archive, report["sha256"].as_str().unwrap(), &restored).unwrap();
    fs::rename(&data, f.temp.path().join("old")).unwrap();
    let source_digest = hash(&restored.join("data/preflight.sqlite3"));
    let plan = b::activation_preview(&restored, &configuration).unwrap();
    b::activate(&restored, &configuration, plan["plan_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        hash(&restored.join("data/preflight.sqlite3")),
        source_digest
    );
    let e = preflight::engine::Engine::open(
        preflight::config::Config::load(&restored.join("server-config.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(e.db.project(&project).unwrap().id, project);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            data.join("preflight.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        )
        .unwrap()
        .query_row("SELECT max(version) FROM schema_version", [], |r| r
            .get::<_, usize>(0))
        .unwrap(),
        preflight::migrations::VERSION
    );
    e.shutdown().await.unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn activation_requires_approved_unchanged_plan_and_never_overwrites_data() {
    let f = common::Fixture::new();
    let mut cfg = f.engine.config.clone();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    cfg.bind = socket.local_addr().unwrap();
    drop(socket);
    let configuration = f.temp.path().join("activation.toml");
    fs::write(&configuration, toml::to_string(&cfg).unwrap()).unwrap();
    let data = cfg.data_dir.clone();
    let project = f.project.id.clone();
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let archive = f.temp.path().join("activation.tar.zst");
    let report = b::create(&configuration, &archive).unwrap();
    let restored = f.temp.path().join("staged");
    b::restore(&archive, report["sha256"].as_str().unwrap(), &restored).unwrap();
    assert!(
        b::activation_preview(&restored, &configuration)
            .unwrap_err()
            .to_string()
            .contains("target exists")
    );
    let preserved = f.temp.path().join("preserved");
    fs::rename(&data, &preserved).unwrap();
    let socket = std::net::TcpListener::bind(cfg.bind).unwrap();
    assert!(
        b::activation_preview(&restored, &configuration)
            .unwrap_err()
            .to_string()
            .contains("address is active")
    );
    drop(socket);
    let plan = b::activation_preview(&restored, &configuration).unwrap();
    let plan_id = plan["plan_id"].as_str().unwrap();
    assert!(!data.exists());
    assert!(b::activate(&restored, &configuration, &"0".repeat(64)).is_err());
    fs::write(restored.join("data/unapproved-file"), b"reject extras").unwrap();
    assert!(b::activate(&restored, &configuration, plan_id).is_err());
    assert!(!data.exists());
    fs::remove_file(restored.join("data/unapproved-file")).unwrap();
    let saved_configuration = fs::read(&configuration).unwrap();
    let mut changed = saved_configuration.clone();
    changed.extend_from_slice(b"\n# changed after preview\n");
    fs::write(&configuration, changed).unwrap();
    assert!(b::activate(&restored, &configuration, plan_id).is_err());
    assert!(!data.exists());
    fs::write(&configuration, saved_configuration).unwrap();
    let result = b::activate(&restored, &configuration, plan_id).unwrap();
    assert_eq!(result["activated"], true);
    assert_eq!(result["already_activated"], false);
    assert!(preserved.join("preflight.sqlite3").is_file());
    assert!(restored.join("data/preflight.sqlite3").is_file());
    let engine =
        preflight::engine::Engine::open(preflight::config::Config::load(&configuration).unwrap())
            .unwrap();
    assert_eq!(engine.db.project(&project).unwrap().id, project);
    assert_eq!(
        b::activate(&restored, &configuration, plan_id).unwrap()["already_activated"],
        true
    );
    assert!(b::activate(&restored, &configuration, &"0".repeat(64)).is_err());
    engine.shutdown().await.unwrap();
    drop(engine);
    let second_archive = f.temp.path().join("second.tar.zst");
    let second = b::create(&configuration, &second_archive).unwrap();
    let second_restore = f.temp.path().join("second-restore");
    b::restore(
        &second_archive,
        second["sha256"].as_str().unwrap(),
        &second_restore,
    )
    .unwrap();
    fs::rename(&data, f.temp.path().join("preserved-again")).unwrap();
    let second_plan = b::activation_preview(&second_restore, &configuration).unwrap();
    let second_id = second_plan["plan_id"].as_str().unwrap();
    b::activate(&second_restore, &configuration, second_id).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(data.join("recovery-activation.json")).unwrap()
        )
        .unwrap()["plan_id"],
        second_id
    );
}
