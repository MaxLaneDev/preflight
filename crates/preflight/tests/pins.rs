mod common;
use common::*;
use preflight::{
    control::SaveDefinition,
    model::*,
    pins::{Inspect, StartPinned},
    pipeline::{ArtifactKind, PipelineFile, StartPipeline},
    profiles::{self, Profile, Revision},
};
use serde_json::json;

fn setup(f: &Fixture) -> Profile {
    std::fs::create_dir(f.project.root.join("sdk")).unwrap();
    std::fs::write(f.project.root.join("sdk/version.txt"), "sdk-v1").unwrap();
    let tool = preflight::pins::inspect(
        &f.engine.config,
        &f.project.root,
        &Inspect {
            project_id: f.project.id.clone(),
            path: f.request(&[], 5).command.program,
            kind: ArtifactKind::Executable,
        },
    )
    .unwrap();
    let sdk = preflight::pins::inspect(
        &f.engine.config,
        &f.project.root,
        &Inspect {
            project_id: f.project.id.clone(),
            path: "sdk".into(),
            kind: ArtifactKind::Directory,
        },
    )
    .unwrap();
    serde_json::from_value(json!({"version":1,"tools":{"runner":{"program":"runner","probe_args":["echo","probe"]}},"env":{"SDKROOT":"${sdk.platform}"},"pins":{"tools":{"runner":{"path":tool["path"],"sha256":tool["sha256"]}},"sdks":{"platform":{"path":"sdk","sha256":sdk["sha256"]}},"target":"test-native","abi":{"format":"v1"}}})).unwrap()
}
fn save(f: &Fixture, name: &str, profile: Profile) {
    profiles::save(
        &f.engine,
        &SaveDefinition {
            project_id: f.project.id.clone(),
            name: name.into(),
            expected_revision: 0,
            definition: profile,
        },
    )
    .unwrap();
}
fn definition(args: serde_json::Value, outputs: serde_json::Value) -> PipelineFile {
    serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"work","command":{"program":"${tool.runner}","args":args},"outputs":outputs}]}})).unwrap()
}
fn start(f: &Fixture, name: &str, definition: PipelineFile) -> Run {
    profiles::start_pinned(
        &f.engine,
        StartPinned {
            project_id: f.project.id.clone(),
            definition,
            profile: Revision {
                name: name.into(),
                revision: 1,
            },
            source: None,
            idempotency_key: None,
        },
    )
    .unwrap()
}
async fn publish(f: &Fixture, name: &str) -> Artifact {
    let run = start(
        f,
        name,
        definition(
            json!(["write"]),
            json!({"file":{"path":"result.txt","type":"file"}}),
        ),
    );
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    preflight::modules::publish(
        &f.engine,
        &preflight::modules::Publish {
            run_id: run.id,
            stage_id: "work".into(),
            artifact_name: "file".into(),
        },
    )
    .unwrap()
}
#[tokio::test]
async fn pinned_copy_sdk_module_compatibility_and_restart() {
    let f = Fixture::new();
    let profile = setup(&f);
    save(&f, "locked", profile);
    let artifact = publish(&f, "locked").await;
    let run = start(
        &f,
        "locked",
        definition(
            json!(["echo", format!("${{module.{}}}", artifact.sha256)]),
            json!({}),
        ),
    );
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    assert!(output(&f.engine, &run.id, "stdout").contains(".preflight/modules"));
    let verified = f.engine.db.toolchain(&run.id).unwrap().unwrap();
    assert_eq!(verified.contract["host_os"], std::env::consts::OS);
    assert_eq!(verified.contract["target"], "test-native");
    assert_eq!(
        std::fs::read_to_string(verified.sdks["platform"].artifact.path.join("version.txt"))
            .unwrap(),
        "sdk-v1"
    );
    let stage_path = run
        .workspace
        .join("stages/work/.preflight/toolchain/sdks/platform/version.txt");
    assert_eq!(std::fs::read_to_string(stage_path).unwrap(), "sdk-v1");
    let contracts = f
        .engine
        .db
        .module_contracts(&preflight::modules::ManifestQuery {
            project_id: f.project.id.clone(),
            sha256: artifact.sha256.clone(),
            offset: 0,
        })
        .unwrap();
    assert_eq!(
        contracts["entries"][0]["status"],
        "verified_declared_contract"
    );
    assert_eq!(
        contracts["entries"][0]["contract_sha256"],
        verified.contract_sha256
    );
    // An unpinned consumer must not silently use a strictly published module.
    let legacy=f.engine.start_pipeline(StartPipeline {project_id:f.project.id.clone(),definition:Some(serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"use","command":{"program":"./helper","args":["echo",format!("${{module.{}}}",artifact.sha256)]}}]}})).unwrap())}).unwrap();
    assert_ne!(
        finished(&f.engine, &legacy.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &legacy.id, "stdout").is_empty());
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(Fixture::config(f.temp.path())).unwrap();
    assert_eq!(
        reopened
            .db
            .toolchain(&run.id)
            .unwrap()
            .unwrap()
            .contract_sha256,
        verified.contract_sha256
    );
    assert_eq!(
        reopened
            .db
            .compatible_module(&run.id, &artifact.sha256)
            .unwrap()
            .run_id,
        artifact.run_id
    );
}
#[tokio::test]
async fn changed_tool_or_sdk_is_refused_before_execution() {
    let f = Fixture::new();
    let profile = setup(&f);
    save(&f, "locked", profile);
    std::fs::write(f.project.root.join("sdk/version.txt"), "sdk-v2").unwrap();
    let run = start(
        &f,
        "locked",
        definition(json!(["echo", "must not run"]), json!({})),
    );
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").is_empty());
    assert!(f.engine.db.toolchain(&run.id).unwrap().is_none());
    std::fs::write(f.project.root.join("sdk/version.txt"), "sdk-v1").unwrap();
    let helper = f.project.root.join(f.request(&[], 5).command.program);
    let mut bytes = std::fs::read(&helper).unwrap();
    bytes.push(0);
    std::fs::write(&helper, bytes).unwrap();
    let run = start(
        &f,
        "locked",
        definition(json!(["echo", "must not run"]), json!({})),
    );
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").is_empty());
    assert!(f.engine.db.toolchain(&run.id).unwrap().is_none());
}
#[tokio::test]
async fn incompatible_target_abi_and_sdk_contracts_fail_before_any_stage() {
    let f = Fixture::new();
    let profile = setup(&f);
    save(&f, "locked", profile.clone());
    let artifact = publish(&f, "locked").await;
    for (name, mut changed) in [("target", profile.clone()), ("abi", profile.clone())] {
        let pins = changed.pins.as_mut().unwrap();
        if name == "target" {
            pins.target = "other-target".into();
        } else {
            pins.abi.insert("format".into(), "v2".into());
        }
        save(&f, name, changed);
        let mut plan = definition(
            json!(["echo", format!("${{module.{}}}", artifact.sha256)]),
            json!({}),
        );
        let mut independent = plan.pipeline.stages[0].clone();
        independent.id = "independent".into();
        independent.command.args = vec!["echo".into(), "must not run".into()];
        plan.pipeline.stages.push(independent);
        let run = start(&f, name, plan);
        assert_ne!(
            finished(&f.engine, &run.id).await.status,
            RunState::Succeeded
        );
        assert!(output(&f.engine, &run.id, "stdout").is_empty());
    }
    std::fs::write(f.project.root.join("sdk/version.txt"), "sdk-v2").unwrap();
    let mut changed = profile.clone();
    let observed = preflight::pins::inspect(
        &f.engine.config,
        &f.project.root,
        &Inspect {
            project_id: f.project.id.clone(),
            path: "sdk".into(),
            kind: ArtifactKind::Directory,
        },
    )
    .unwrap();
    changed
        .pins
        .as_mut()
        .unwrap()
        .sdks
        .get_mut("platform")
        .unwrap()
        .sha256 = observed["sha256"].as_str().unwrap().into();
    save(&f, "new-sdk", changed);
    let run = start(
        &f,
        "new-sdk",
        definition(
            json!(["echo", format!("${{module.{}}}", artifact.sha256)]),
            json!({}),
        ),
    );
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").is_empty());
    std::fs::write(f.project.root.join("sdk/version.txt"), "sdk-v1").unwrap();
    let helper = f.project.root.join(f.request(&[], 5).command.program);
    let mut bytes = std::fs::read(&helper).unwrap();
    bytes.push(0);
    std::fs::write(&helper, bytes).unwrap();
    let observed = preflight::pins::inspect(
        &f.engine.config,
        &f.project.root,
        &Inspect {
            project_id: f.project.id.clone(),
            path: helper.to_string_lossy().into_owned(),
            kind: ArtifactKind::Executable,
        },
    )
    .unwrap();
    let mut changed = profile.clone();
    changed
        .pins
        .as_mut()
        .unwrap()
        .tools
        .get_mut("runner")
        .unwrap()
        .sha256 = observed["sha256"].as_str().unwrap().into();
    save(&f, "new-tool", changed);
    let run = start(
        &f,
        "new-tool",
        definition(
            json!(["echo", format!("${{module.{}}}", artifact.sha256)]),
            json!({}),
        ),
    );
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(f.engine.db.toolchain(&run.id).unwrap().is_some());
    assert!(output(&f.engine, &run.id, "stdout").is_empty());
    let mut wrong_host = profile;
    wrong_host.host_arch = Some("other_host".into());
    save(&f, "wrong-host", wrong_host);
    assert!(
        profiles::start_pinned(
            &f.engine,
            StartPinned {
                project_id: f.project.id.clone(),
                definition: definition(json!([]), json!({})),
                profile: Revision {
                    name: "wrong-host".into(),
                    revision: 1
                },
                source: None,
                idempotency_key: None
            }
        )
        .is_err()
    );
}
#[tokio::test]
async fn legacy_runs_remain_unverified_and_inspection_cannot_escape_authority() {
    let f = Fixture::new();
    let run = f.start(&["write"], 5);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(f.engine.db.toolchain(&run.id).unwrap().is_none());
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("input"), "outside").unwrap();
    assert!(
        preflight::pins::inspect(
            &f.engine.config,
            &f.project.root,
            &Inspect {
                project_id: f.project.id.clone(),
                path: outside.path().join("input").to_string_lossy().into_owned(),
                kind: ArtifactKind::File
            }
        )
        .is_err()
    );
    assert!(
        preflight::pins::inspect(
            &f.engine.config,
            &f.project.root,
            &Inspect {
                project_id: f.project.id.clone(),
                path: f.engine.config.data_dir.to_string_lossy().into_owned(),
                kind: ArtifactKind::Directory
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn identical_bytes_keep_distinct_contract_producers_and_cancellation() {
    let f = Fixture::new();
    let profile = setup(&f);
    save(&f, "v1", profile.clone());
    let first = publish(&f, "v1").await;
    let mut second_profile = profile;
    second_profile
        .pins
        .as_mut()
        .unwrap()
        .abi
        .insert("format".into(), "v2".into());
    save(&f, "v2", second_profile);
    let run = start(
        &f,
        "v2",
        definition(
            json!(["write"]),
            json!({"file":{"path":"result.txt","type":"file"}}),
        ),
    );
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let second = preflight::modules::publish(
        &f.engine,
        &preflight::modules::Publish {
            run_id: run.id.clone(),
            stage_id: "work".into(),
            artifact_name: "file".into(),
        },
    )
    .unwrap();
    assert_eq!(first.sha256, second.sha256);
    let consumer = start(
        &f,
        "v2",
        definition(
            json!(["echo", format!("${{module.{}}}", first.sha256)]),
            json!({}),
        ),
    );
    assert_eq!(
        finished(&f.engine, &consumer.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine
            .db
            .compatible_module(&consumer.id, &first.sha256)
            .unwrap()
            .run_id,
        run.id
    );
    let page = f
        .engine
        .db
        .module_contracts(&preflight::modules::ManifestQuery {
            project_id: f.project.id.clone(),
            sha256: first.sha256,
            offset: 0,
        })
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 2);
    let cancelled = start(&f, "v1", definition(json!(["echo", "never"]), json!({})));
    f.engine
        .cancel(CancelRun {
            run_id: cancelled.id.clone(),
            actor: Actor::Agent,
            reason: "pinned queued cancel".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &cancelled.id).await.status,
        RunState::Cancelled
    );
    assert!(f.engine.db.toolchain(&cancelled.id).unwrap().is_none());
}

#[test]
fn pin_schema_requires_complete_tools_and_preserves_legacy_encoding() {
    let old = json!({"version":1,"tools":{"runner":{"program":"runner","args_prefix":[],"probe_args":null}},"env":{},"host_os":null,"host_arch":null});
    let profile: Profile = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(&profile).unwrap(), old);
    let mut pinned = profile;
    pinned.pins = Some(preflight::pins::Pins {
        tools: Default::default(),
        sdks: Default::default(),
        target: "host".into(),
        abi: Default::default(),
    });
    assert!(profiles::validate(&pinned).is_err());
    pinned.pins.as_mut().unwrap().tools.insert(
        "runner".into(),
        preflight::pins::Pin {
            path: "runner".into(),
            sha256: "a".repeat(64),
        },
    );
    profiles::validate(&pinned).unwrap();
    pinned
        .pins
        .as_mut()
        .unwrap()
        .tools
        .get_mut("runner")
        .unwrap()
        .sha256 = "bad".into();
    assert!(profiles::validate(&pinned).is_err());
}

fn cached_definition() -> PipelineFile {
    let mut file = definition(
        json!(["write"]),
        json!({"file":{"path":"result.txt","type":"file"}}),
    );
    file.pipeline.stages[0].cache = Some(preflight::cache::Policy {
        single_flight: false,
        pipeline_id: "compiler".into(),
        deterministic: true,
        inputs_complete: true,
    });
    file
}
fn cache_status(f: &Fixture, run: &str) -> serde_json::Value {
    f.engine.db.cache_decisions(run).unwrap()["stages"][0]["decision"].clone()
}
#[tokio::test]
async fn cache_explains_different_patches_with_identical_final_source_bytes() {
    use preflight::sources::{GitSource, Patch, SourceRequest};
    let f = Fixture::new();
    let profile = setup(&f);
    std::fs::write(f.project.root.join("note.txt"), "base\n").unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Preflight test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    let commit = git(&["rev-parse", "HEAD"]).trim().to_owned();
    save(&f, "pinned", profile);
    let patch = |before: &str, after: &str| Patch {
        name: format!("{before}-to-{after}"),
        diff: format!(
            "diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-{before}\n+{after}\n"
        ),
    };
    let launch = |patches| {
        profiles::start_pinned(
            &f.engine,
            StartPinned {
                project_id: f.project.id.clone(),
                definition: cached_definition(),
                profile: Revision {
                    name: "pinned".into(),
                    revision: 1,
                },
                source: Some(SourceRequest::Git(GitSource {
                    commit: commit.clone(),
                    patches,
                })),
                idempotency_key: None,
            },
        )
        .unwrap()
    };
    let first = launch(vec![patch("base", "final")]);
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Succeeded
    );
    let second = launch(vec![patch("base", "middle"), patch("middle", "final")]);
    assert_eq!(
        finished(&f.engine, &second.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &first.id)["status"], "miss");
    assert_eq!(cache_status(&f, &second.id)["status"], "miss");
    let explanation = preflight::stage_keys::explain(
        &f.engine.db,
        &preflight::stage_keys::Query {
            run_id: second.id.clone(),
            stage_id: "work".into(),
            baseline_id: Some(first.id.clone()),
        },
    )
    .unwrap();
    assert_eq!(explanation["changed_components"], json!([]));
    assert_eq!(
        explanation["changed_cache_components"],
        json!(["source_provenance"])
    );
    assert_eq!(
        explanation["build_cache_identity"]["key"],
        cache_status(&f, &second.id)["key"]
    );
    assert_eq!(
        explanation["baseline_build_cache_identity"]["key"],
        cache_status(&f, &first.id)["key"]
    );
    assert_eq!(
        explanation["build_cache_identity"]["reuse_authorized"],
        false
    );
    let third = launch(vec![patch("base", "middle"), patch("middle", "final")]);
    assert_eq!(
        finished(&f.engine, &third.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &third.id)["status"], "hit");
    assert_eq!(cache_status(&f, &third.id)["producer_run"], second.id);
    assert!(
        !serde_json::to_string(&explanation)
            .unwrap()
            .contains(f.project.root.to_str().unwrap())
    );
    let db =
        rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    db.execute("DELETE FROM stage_keys WHERE run_id=?1", [&third.id])
        .unwrap();
    let missing = preflight::stage_keys::explain(
        &f.engine.db,
        &preflight::stage_keys::Query {
            run_id: third.id,
            stage_id: "work".into(),
            baseline_id: Some(first.id),
        },
    )
    .unwrap();
    assert!(missing["build_cache_identity"].is_null());
    assert!(missing["changed_cache_components"].is_null());
    assert_eq!(
        std::fs::read_to_string(f.project.root.join("note.txt")).unwrap(),
        "base\n"
    );
}
#[tokio::test]
async fn cache_identity_separates_command_environment_and_target_contract() {
    let f = Fixture::new();
    let profile = setup(&f);
    save(&f, "pinned", profile.clone());
    let baseline = start(&f, "pinned", cached_definition());
    assert_eq!(
        finished(&f.engine, &baseline.id).await.status,
        RunState::Succeeded
    );
    let mut target = profile;
    target.pins.as_mut().unwrap().target = "another-target".into();
    save(&f, "another", target);
    let mut command = cached_definition();
    command.pipeline.stages[0]
        .command
        .args
        .push("changed-flag".into());
    let mut environment = cached_definition();
    environment.pipeline.stages[0]
        .env
        .insert("BUILD_MODE".into(), "release".into());
    for (profile, file, component) in [
        ("pinned", command, "stage"),
        ("pinned", environment, "stage"),
        ("another", cached_definition(), "toolchain"),
    ] {
        let run = start(&f, profile, file.clone());
        assert_eq!(
            finished(&f.engine, &run.id).await.status,
            RunState::Succeeded
        );
        assert_eq!(cache_status(&f, &run.id)["status"], "miss");
        let explanation = preflight::stage_keys::explain(
            &f.engine.db,
            &preflight::stage_keys::Query {
                run_id: run.id.clone(),
                stage_id: "work".into(),
                baseline_id: Some(baseline.id.clone()),
            },
        )
        .unwrap();
        assert!(
            explanation["changed_components"]
                .as_array()
                .unwrap()
                .contains(&json!(component)),
            "{explanation}"
        );
        assert_eq!(
            explanation["changed_cache_components"],
            json!(["stage_evidence"])
        );
        assert_ne!(
            cache_status(&f, &run.id)["key"],
            cache_status(&f, &baseline.id)["key"]
        );
        let repeated = start(&f, profile, file);
        assert_eq!(
            finished(&f.engine, &repeated.id).await.status,
            RunState::Succeeded
        );
        assert_eq!(cache_status(&f, &repeated.id)["status"], "hit");
        assert_eq!(cache_status(&f, &repeated.id)["producer_run"], run.id);
    }
}
#[tokio::test]
async fn cache_rejects_ambient_path_launchers_even_with_an_unrelated_pinned_profile() {
    let f = Fixture::new();
    save(&f, "locked", setup(&f));
    let mut definition = cached_definition();
    definition.pipeline.stages[0].command.program = "ambient-launcher".into();
    let before = f.engine.db.runs().unwrap().len();
    let error = profiles::start_pinned(
        &f.engine,
        StartPinned {
            project_id: f.project.id.clone(),
            definition,
            profile: Revision {
                name: "locked".into(),
                revision: 1,
            },
            source: None,
            idempotency_key: None,
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("pinned stages must use declared tool, artifact or module programs")
    );
    assert_eq!(
        f.engine.db.runs().unwrap().len(),
        before,
        "invalid cached launches must be rejected before run admission"
    );
}
#[tokio::test]
async fn cache_reuses_verified_private_outputs_and_rebuilds_corruption() {
    let f = Fixture::new();
    save(&f, "pinned", setup(&f));
    let a = start(&f, "pinned", cached_definition());
    assert_eq!(finished(&f.engine, &a.id).await.status, RunState::Succeeded);
    assert_eq!(cache_status(&f, &a.id)["status"], "miss");
    assert!(!output(&f.engine, &a.id, "stdout").is_empty());
    let b = start(&f, "pinned", cached_definition());
    assert_eq!(finished(&f.engine, &b.id).await.status, RunState::Succeeded);
    assert_eq!(cache_status(&f, &b.id)["status"], "hit");
    assert_eq!(cache_status(&f, &b.id)["producer_run"], a.id);
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    let (hits,last_used,rebuild):(u64,i64,u64)=c.query_row("SELECT a.hits,a.last_used,a.rebuild_ms FROM cache_access a JOIN stage_cache c ON c.id=a.cache_id WHERE c.producer_run=?1",[&a.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(hits, 1);
    assert!(last_used >= f.engine.db.run(&a.id).unwrap().created_at);
    assert!(rebuild > 0);

    assert!(output(&f.engine, &b.id, "stdout").is_empty());
    let source = f.engine.db.artifacts(&a.id).unwrap().remove(0);
    let copied = f.engine.db.artifacts(&b.id).unwrap().remove(0);
    assert_ne!(source.path, copied.path);
    assert_eq!(source.sha256, copied.sha256);
    std::fs::remove_file(&source.path).unwrap();
    std::fs::write(&source.path, "corrupted").unwrap();
    let c = start(&f, "pinned", cached_definition());
    assert_eq!(finished(&f.engine, &c.id).await.status, RunState::Succeeded);
    assert_eq!(cache_status(&f, &c.id)["status"], "invalidated_miss");
    assert!(!output(&f.engine, &c.id, "stdout").is_empty());
    assert_eq!(
        std::fs::read(&copied.path).unwrap(),
        std::fs::read(f.engine.db.artifacts(&c.id).unwrap()[0].path.clone()).unwrap()
    );
    std::fs::write(f.project.root.join("changed-input"), "changed").unwrap();
    let changed = start(&f, "pinned", cached_definition());
    assert_eq!(
        finished(&f.engine, &changed.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &changed.id)["status"], "miss");
}
#[tokio::test]
async fn cache_scope_restart_and_explicit_policy() {
    let f = Fixture::new();
    save(&f, "pinned", setup(&f));
    let a = start(&f, "pinned", cached_definition());
    assert_eq!(finished(&f.engine, &a.id).await.status, RunState::Succeeded);
    f.engine.shutdown().await.unwrap();
    let Fixture {
        temp,
        engine,
        project,
    } = f;
    let config = engine.config.clone();
    drop(engine);
    let f = Fixture {
        temp,
        project,
        engine: preflight::engine::Engine::open(config).unwrap(),
    };
    let b = start(&f, "pinned", cached_definition());
    assert_eq!(finished(&f.engine, &b.id).await.status, RunState::Succeeded);
    assert_eq!(cache_status(&f, &b.id)["status"], "hit");
    let mut other_scope = cached_definition();
    other_scope.pipeline.stages[0]
        .cache
        .as_mut()
        .unwrap()
        .pipeline_id = "other".into();
    let other = start(&f, "pinned", other_scope);
    assert_eq!(
        finished(&f.engine, &other.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &other.id)["status"], "miss");
    let root = f.temp.path().join("other-project");
    std::fs::create_dir_all(root.join("sdk")).unwrap();
    std::fs::write(root.join("sdk/version.txt"), "sdk-v1").unwrap();
    let program = f.request(&[], 5).command.program;
    std::fs::copy(f.project.root.join(&program), root.join(&program)).unwrap();
    let other_project = f
        .engine
        .register(RegisterProject {
            name: "other".into(),
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let saved = profiles::get(
        &f.engine,
        &preflight::control::ConfigRef {
            project_id: f.project.id.clone(),
            name: "pinned".into(),
            revision: Some(1),
        },
    )
    .unwrap();
    profiles::save(
        &f.engine,
        &SaveDefinition {
            project_id: other_project.id.clone(),
            name: "pinned".into(),
            expected_revision: 0,
            definition: serde_json::from_value::<Profile>(saved["definition"].clone()).unwrap(),
        },
    )
    .unwrap();
    let other = profiles::start_pinned(
        &f.engine,
        StartPinned {
            project_id: other_project.id,
            definition: cached_definition(),
            profile: Revision {
                name: "pinned".into(),
                revision: 1,
            },
            source: None,
            idempotency_key: None,
        },
    )
    .unwrap();
    assert_eq!(
        finished(&f.engine, &other.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &other.id)["status"], "miss");
    assert_eq!(
        cache_status(&f, &other.id)["key"],
        cache_status(&f, &a.id)["key"]
    );
    assert!(
        f.engine
            .start_pipeline(StartPipeline {
                project_id: f.project.id.clone(),
                definition: Some(cached_definition())
            })
            .is_err()
    );
    let mut invalid = cached_definition();
    invalid.pipeline.stages[0].kind = preflight::pipeline::StageKind::Test;
    assert!(preflight::pipeline::validate(&invalid).is_err());
    invalid.pipeline.stages[0].kind = preflight::pipeline::StageKind::Build;
    invalid.pipeline.stages[0]
        .cache
        .as_mut()
        .unwrap()
        .inputs_complete = false;
    assert!(preflight::pipeline::validate(&invalid).is_err());
    let uncached = start(
        &f,
        "pinned",
        definition(
            json!(["write"]),
            json!({"file":{"path":"result.txt","type":"file"}}),
        ),
    );
    assert_eq!(
        finished(&f.engine, &uncached.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &uncached.id)["status"], "disabled");
}

#[tokio::test]
async fn cache_does_not_reuse_successful_stage_from_failed_run() {
    let f = Fixture::new();
    save(&f, "pinned", setup(&f));
    let mut file = cached_definition();
    let mut failure = preflight::pipeline::StageDefinition::native(CommandSpec {
        program: "${tool.runner}".into(),
        args: vec!["fail".into()],
        timeout_seconds: 5,
    });
    failure.id = "fail".into();
    failure.needs = vec!["work".into()];
    file.pipeline.stages.push(failure);
    for _ in 0..2 {
        let run = start(&f, "pinned", file.clone());
        assert_eq!(finished(&f.engine, &run.id).await.status, RunState::Failed);
        let decisions = f.engine.db.cache_decisions(&run.id).unwrap();
        let work = decisions["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["stage_id"] == "work")
            .unwrap();
        assert_eq!(work["decision"]["status"], "miss");
    }
}

#[tokio::test]
async fn cache_cleanup_rebuilds_without_deleting_producer_or_shared_module() {
    let f = Fixture::new();
    save(&f, "pinned", setup(&f));
    let producer = start(&f, "pinned", cached_definition());
    assert_eq!(
        finished(&f.engine, &producer.id).await.status,
        RunState::Succeeded
    );
    let artifact = f.engine.db.artifacts(&producer.id).unwrap().remove(0);
    let module = preflight::modules::publish(
        &f.engine,
        &preflight::modules::Publish {
            run_id: producer.id.clone(),
            stage_id: "work".into(),
            artifact_name: "file".into(),
        },
    )
    .unwrap();
    for _ in 0..100 {
        if f.engine.db.operations(&producer.id).unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["state"] == "released")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let plan = f
        .engine
        .db
        .cache_cleanup_preview(&preflight::cache::CleanupPreview {
            project_id: f.project.id.clone(),
            pipeline_id: Some("compiler".into()),
        })
        .unwrap();
    assert_eq!(plan["total"], 1);
    let execute = preflight::cache::CleanupExecute {
        plan_id: plan["plan_id"].as_str().unwrap().into(),
    };
    let result = f.engine.db.cache_cleanup_execute(&execute).unwrap();
    assert_eq!(result["invalidated"].as_array().unwrap().len(), 1);
    assert_eq!(result["physical_bytes_deleted"], 0);
    assert_eq!(result, f.engine.db.cache_cleanup_execute(&execute).unwrap());
    assert_eq!(
        f.engine.db.run(&producer.id).unwrap().status,
        RunState::Succeeded
    );
    assert!(artifact.path.is_file());
    assert!(module.path.is_file());
    assert_eq!(
        f.engine
            .db
            .module(&f.project.id, &module.sha256)
            .unwrap()
            .sha256,
        module.sha256
    );
    let rebuilt = start(&f, "pinned", cached_definition());
    assert_eq!(
        finished(&f.engine, &rebuilt.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &rebuilt.id)["status"], "miss");
    assert!(!output(&f.engine, &rebuilt.id, "stdout").is_empty());
}

fn shared_setup(f: &Fixture) -> PipelineFile {
    static BINARY: std::sync::OnceLock<(tempfile::TempDir, std::path::PathBuf)> =
        std::sync::OnceLock::new();
    let binary = &BINARY
        .get_or_init(|| {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("shared-worker.exe");
            let result = std::process::Command::new("rustc")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/support/shared_worker.rs"
                ))
                .arg("-o")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            (tmp, path)
        })
        .1;
    std::fs::copy(
        binary,
        f.project.root.join(if cfg!(windows) {
            "helper.exe"
        } else {
            "helper"
        }),
    )
    .unwrap();
    let mut profile = setup(f);
    profile.tools.get_mut("runner").unwrap().probe_args = None;
    save(f, "shared", profile);
    let mut file = cached_definition();
    file.pipeline.stages[0].command.args = vec![
        f.temp.path().join("calls").to_string_lossy().into(),
        "1500".into(),
        "write".into(),
    ];
    file.pipeline.stages[0]
        .cache
        .as_mut()
        .unwrap()
        .single_flight = true;
    file
}
async fn sharing(f: &Fixture, id: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let decisions = f.engine.db.cache_decisions(id).unwrap();
            if let Some(id) = decisions["stages"][0]["decision"]["execution_run"].as_str() {
                break id.to_owned();
            }
            let run = f.engine.db.run(id).unwrap();
            assert!(
                !run.status.terminal(),
                "run terminated before sharing: {run:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn single_flight_first_subscriber_cancel_preserves_second_and_one_process() {
    let f = Fixture::new();
    let file = shared_setup(&f);
    let first = start(&f, "shared", file.clone());
    let execution = sharing(&f, &first.id).await;
    let second = start(&f, "shared", file);
    assert_eq!(sharing(&f, &second.id).await, execution);
    assert!(
        f.engine
            .cancel(CancelRun {
                run_id: execution.clone(),
                actor: Actor::User,
                reason: "cannot cancel shared process directly".into()
            })
            .is_err()
    );
    f.engine
        .cancel(CancelRun {
            run_id: first.id.clone(),
            actor: Actor::User,
            reason: "detach initiator".into(),
        })
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            finished(&f.engine, &first.id)
        )
        .await
        .unwrap()
        .status,
        RunState::Cancelled
    );
    assert_eq!(
        finished(&f.engine, &second.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine.db.run(&execution).unwrap().status,
        RunState::Succeeded
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "executed\n"
    );
    let outputs = f.engine.db.artifacts(&second.id).unwrap();
    assert_eq!(
        std::fs::read_to_string(&outputs[0].path).unwrap(),
        "shared output"
    );
    assert!(f.engine.db.artifacts(&first.id).unwrap().is_empty());
    assert_eq!(f.engine.db.runs().unwrap().len(), 2);
    let events = f
        .engine
        .db
        .completion_events(&preflight::events::Query {
            project_id: f.project.id.clone(),
            after: 0,
            limit: 100,
        })
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["run_id"] != execution)
    );
    assert_eq!(cache_status(&f, &second.id)["status"], "shared");
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn single_flight_last_cancel_stops_execution_and_next_run_can_rebuild() {
    let f = Fixture::new();
    let mut file = shared_setup(&f);
    file.pipeline.stages[0].command.args[1] = "5000".into();
    let first = start(&f, "shared", file.clone());
    let execution = sharing(&f, &first.id).await;
    let second = start(&f, "shared", file.clone());
    assert_eq!(sharing(&f, &second.id).await, execution);
    for run in [&first, &second] {
        f.engine
            .cancel(CancelRun {
                run_id: run.id.clone(),
                actor: Actor::User,
                reason: "detach all".into(),
            })
            .unwrap();
    }
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        finished(&f.engine, &second.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finished(&f.engine, &execution)
        )
        .await
        .unwrap()
        .status,
        RunState::Cancelled
    );
    // Same key must not join a cancelled flight; all-waiter cancellation removed it.
    let next = start(&f, "shared", file);
    assert_ne!(sharing(&f, &next.id).await, execution);
    f.engine
        .cancel(CancelRun {
            run_id: next.id.clone(),
            actor: Actor::User,
            reason: "end test".into(),
        })
        .unwrap();
    finished(&f.engine, &next.id).await;
    f.engine.shutdown().await.unwrap();
    assert_eq!(f.engine.scheduler_status()["reserved_slots"], 0);
}
#[tokio::test]
async fn single_flight_failures_are_not_cached_and_subscribers_recover_after_restart() {
    let f = Fixture::new();
    let mut file = shared_setup(&f);
    file.pipeline.stages[0].command.args[2] = "fail".into();
    let first = start(&f, "shared", file.clone());
    let execution = sharing(&f, &first.id).await;
    let second = start(&f, "shared", file);
    assert_eq!(sharing(&f, &second.id).await, execution);
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Failed
    );
    assert_eq!(
        finished(&f.engine, &second.id).await.status,
        RunState::Failed
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "executed\n"
    );
    f.engine.shutdown().await.unwrap();
    let config = f.engine.config.clone();
    let connection = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM stage_cache", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection
        .execute("UPDATE shared_subscribers SET state='waiting'", [])
        .unwrap();
    drop(connection);
    drop(f.engine);
    let engine = preflight::engine::Engine::open(config.clone()).unwrap();
    let connection = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM shared_subscribers WHERE state='abandoned'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(engine.db.runs().unwrap().len(), 2);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn single_flight_second_cancel_preserves_owner_then_cache_and_key_separation() {
    let f = Fixture::new();
    let file = shared_setup(&f);
    let first = start(&f, "shared", file.clone());
    let execution = sharing(&f, &first.id).await;
    let second = start(&f, "shared", file.clone());
    assert_eq!(sharing(&f, &second.id).await, execution);
    f.engine
        .cancel(CancelRun {
            run_id: second.id.clone(),
            actor: Actor::User,
            reason: "detach follower".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &second.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Succeeded
    );
    let report = preflight::single_flight::inspect(&f.engine, &first.id).unwrap();
    assert_eq!(report["stages"][0]["execution_run"], execution);
    assert_eq!(report["stages"][0]["subscription"], "succeeded");
    let hit = start(&f, "shared", file.clone());
    assert_eq!(
        finished(&f.engine, &hit.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(cache_status(&f, &hit.id)["status"], "hit");
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "executed\n"
    );
    let mut changed = file;
    changed.pipeline.stages[0].command.args[1] = "20".into();
    let new = start(&f, "shared", changed);
    assert_eq!(
        finished(&f.engine, &new.id).await.status,
        RunState::Succeeded
    );
    assert_ne!(cache_status(&f, &new.id)["execution_run"], execution);
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "executed\nexecuted\n"
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_run_in_place_uses_original_verified_path() {
    let f = Fixture::new();
    let mut profile = setup(&f);
    profile.tools.get_mut("runner").unwrap().run_in_place = true;
    save(&f, "native-original", profile);
    let run = start(
        &f,
        "native-original",
        definition(json!(["echo", "${pinned.runner}"]), json!({})),
    );
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    let verified = f.engine.db.toolchain(&run.id).unwrap().unwrap();
    assert!(verified.in_place_tools.contains("runner"));
    let compact = preflight::api::compact_report(&f.engine, &run.id).unwrap();
    assert!(compact["submission"].get("profile_snapshot").is_none());
    assert!(compact["submission"]["request"].get("definition").is_none());
    assert_eq!(compact["submission"]["definition_tool"], "runs_definition");
    let original = &verified.tools["runner"].source_path;
    assert_eq!(
        verified.contract["in_place_tools"]["runner"],
        original.to_string_lossy().as_ref()
    );
    assert!(output(&f.engine, &run.id, "stdout").contains(original.to_string_lossy().as_ref()));
    let unpinned=f.engine.start_pipeline(StartPipeline{project_id:f.project.id.clone(),definition:Some(serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"not-authorized","command":{"program":original,"args":[]}}]}})).unwrap())});
    assert!(unpinned.is_err());
    f.engine.shutdown().await.unwrap();
}
