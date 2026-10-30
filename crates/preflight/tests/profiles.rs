mod common;
use common::*;
use preflight::{
    control::{ConfigRef, SavePipeline},
    model::*,
    profiles::{self, Probe, Profile, Revision, SaveProfile, SavedStart},
};
use serde_json::json;
fn setup(f: &Fixture) -> SavedStart {
    assert!(f.temp.path().exists());
    let profile:Profile=serde_json::from_value(json!({"version":1,"tools":{"runner":{"program":f.request(&[],5).command.program,"probe_args":[]}},"env":{"PROFILE_VALUE":"default","OVERRIDE":"default"}})).unwrap();
    profiles::save(
        &f.engine,
        &SaveProfile {
            project_id: f.project.id.clone(),
            name: "any-tools".into(),
            expected_revision: 0,
            definition: profile,
        },
    )
    .unwrap();
    f.engine.db.save_pipeline(&SavePipeline{project_id:f.project.id.clone(),name:"generic".into(),expected_revision:0,definition:serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"work","command":{"program":"${tool.runner}","args":[]},"env":{"OVERRIDE":"stage"}}]}})).unwrap()}).unwrap();
    SavedStart {
        project_id: f.project.id.clone(),
        name: "generic".into(),
        revision: Some(1),
        profile: Some(Revision {
            name: "any-tools".into(),
            revision: 1,
        }),
        idempotency_key: Some("same-request".into()),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_duplicate_requests_are_one_run_and_survive_restart() {
    let f = Fixture::new();
    let request = setup(&f);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let engine = f.engine.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            profiles::start(&engine, request).unwrap().id
        });
    }
    let mut ids = std::collections::BTreeSet::new();
    while let Some(r) = tasks.join_next().await {
        ids.insert(r.unwrap());
    }
    assert_eq!(ids.len(), 1);
    let id = ids.first().unwrap();
    let run = finished(&f.engine, id).await;
    assert_eq!(run.status, RunState::Succeeded);
    assert_eq!(f.engine.db.runs().unwrap().len(), 1);
    assert_eq!(run.stage.definition.env["PROFILE_VALUE"], "default");
    assert_eq!(run.stage.definition.env["OVERRIDE"], "stage");
    assert!(!output(&f.engine, id, "stdout").is_empty());
    let mut other = request.clone();
    other.profile = None;
    assert!(profiles::start(&f.engine, other).is_err());
    let metadata = f.engine.db.submission(id).unwrap().unwrap();
    assert!(metadata["profile_sha256"].as_str().is_some());
    f.engine.shutdown().await.unwrap();
    let config = f.engine.config.clone();
    drop(f.engine);
    let e = preflight::engine::Engine::open(config).unwrap();
    // Replaying an accepted request must not depend on mutable local config files.
    std::fs::remove_file(
        f.project
            .root
            .join(".preflight/toolchains/any-tools/1.json"),
    )
    .unwrap();
    let repeated = profiles::start(&e, request).unwrap();
    assert_eq!(repeated.id, *id);
    assert_eq!(e.db.runs().unwrap().len(), 1);
    e.shutdown().await.unwrap();
}
#[tokio::test]
async fn profiles_probe_validate_and_preserve_revisions() {
    let f = Fixture::new();
    let request = setup(&f);
    let reference = ConfigRef {
        project_id: f.project.id.clone(),
        name: "any-tools".into(),
        revision: Some(1),
    };
    let saved = profiles::get(&f.engine, &reference).unwrap();
    let profile: Profile = serde_json::from_value(saved["definition"].clone()).unwrap();
    assert!(
        f.project
            .root
            .join(".preflight/toolchains/any-tools/1.json")
            .is_file()
    );
    assert_eq!(
        profiles::list(&f.engine, &f.project.id).unwrap()["toolchains"][0]["name"],
        "any-tools"
    );
    let probe = Probe {
        project_id: f.project.id.clone(),
        name: "any-tools".into(),
        revision: 1,
        idempotency_key: Some("probe".into()),
    };
    let first = profiles::probe(&f.engine, probe.clone()).unwrap();
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(profiles::probe(&f.engine, probe).unwrap().id, first.id);
    let mut wrong = profile.clone();
    wrong.host_arch = Some("different_arch".into());
    profiles::save(
        &f.engine,
        &SaveProfile {
            project_id: f.project.id.clone(),
            name: "any-tools".into(),
            expected_revision: 1,
            definition: wrong,
        },
    )
    .unwrap();
    let mut r = request.clone();
    r.idempotency_key = Some("different".into());
    r.profile.as_mut().unwrap().revision = 2;
    assert!(profiles::start(&f.engine, r).is_err());
    let good = profiles::start(&f.engine, request).unwrap();
    assert_eq!(
        finished(&f.engine, &good.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(profiles::get(&f.engine, &reference).unwrap(), saved);
    let mut bad = profile;
    bad.env.insert("PREFLIGHT_WORKSPACE".into(), "bad".into());
    assert!(profiles::validate(&bad).is_err());
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelled_duplicate_does_not_restart_and_new_key_can_run() {
    let f = Fixture::new();
    let legacy = f.start(&[], 5);
    finished(&f.engine, &legacy.id).await;
    let mut request = setup(&f);
    let mut pipeline = f
        .engine
        .db
        .pipeline_config(&ConfigRef {
            project_id: f.project.id.clone(),
            name: "generic".into(),
            revision: Some(1),
        })
        .unwrap()["definition"]
        .clone();
    pipeline["pipeline"]["stages"][0]["command"]["args"] = json!(["sleep"]);
    f.engine
        .db
        .save_pipeline(&SavePipeline {
            project_id: f.project.id.clone(),
            name: "generic".into(),
            expected_revision: 1,
            definition: serde_json::from_value(pipeline).unwrap(),
        })
        .unwrap();
    request.revision = Some(2);
    let first = profiles::start(&f.engine, request.clone()).unwrap();
    f.engine
        .cancel(CancelRun {
            run_id: first.id.clone(),
            actor: Actor::User,
            reason: "test".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        profiles::start(&f.engine, request.clone()).unwrap().id,
        first.id
    );
    request.idempotency_key = Some("new-attempt".into());
    let second = profiles::start(&f.engine, request).unwrap();
    assert_ne!(second.id, first.id);
    f.engine.shutdown().await.unwrap();
    let legacy = f.engine.db.run(&first.id).unwrap();
    assert_eq!(legacy.status, RunState::Cancelled);
}
