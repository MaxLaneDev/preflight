mod common;
use common::*;
use preflight::{control::*, model::RunState, pipeline::StartPipeline};
use serde_json::json;
#[tokio::test]
async fn revisions_conflicts_export_history_and_restart() {
    let f = Fixture::new();
    assert!(f.temp.path().exists());
    let definition=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"test","command":f.request(&[],5).command}]}})).unwrap();
    let mut save = SavePipeline {
        project_id: f.project.id.clone(),
        name: "checks".into(),
        expected_revision: 0,
        definition,
    };
    let first = f.engine.db.save_pipeline(&save).unwrap();
    std::fs::write(
        f.project.root.join(".preflight/pipeline.yaml"),
        serde_json::to_vec(&save.definition).unwrap(),
    )
    .unwrap();
    assert_eq!(
        f.engine
            .configured_pipeline(&f.project.id)
            .unwrap()
            .pipeline
            .stages[0]
            .id,
        "test"
    );

    assert_eq!(first["revision"], 1);
    assert!(
        f.project
            .root
            .join(".preflight/pipelines/checks/1.json")
            .exists()
    );
    assert!(
        std::fs::read_to_string(f.project.root.join(".gitignore"))
            .unwrap()
            .contains("/.preflight/")
    );
    assert!(f.engine.db.save_pipeline(&save).is_err());
    save.expected_revision = 1;
    save.definition.pipeline.stages[0].command.args = vec!["fail".into()];
    f.engine.db.save_pipeline(&save).unwrap();
    let r = ConfigRef {
        project_id: f.project.id.clone(),
        name: "checks".into(),
        revision: Some(1),
    };
    let good = start_saved(&f.engine, r.clone()).unwrap();
    assert_eq!(
        finished(&f.engine, &good.id).await.status,
        RunState::Succeeded
    );
    let bad = start_saved(
        &f.engine,
        ConfigRef {
            revision: Some(2),
            ..r.clone()
        },
    )
    .unwrap();
    assert_eq!(finished(&f.engine, &bad.id).await.status, RunState::Failed);
    assert!(!output(&f.engine, &good.id, "stdout").is_empty());
    let legacy = f.start(&[], 5);
    finished(&f.engine, &legacy.id).await;
    let page = f
        .engine
        .db
        .query_runs(&RunQuery {
            project_id: Some(f.project.id.clone()),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    let next = f
        .engine
        .db
        .query_runs(&RunQuery {
            before: Some(page["next_cursor"].as_str().unwrap().into()),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(page["runs"][0]["run_id"], next["runs"][0]["run_id"]);
    assert!(
        start_saved(
            &f.engine,
            ConfigRef {
                revision: None,
                ..r.clone()
            }
        )
        .is_err()
    );
    let exported = f.engine.db.pipeline_config(&r).unwrap();
    assert_eq!(exported["sha256"], first["sha256"]);
    f.engine.shutdown().await.unwrap();
    let config = f.engine.config.clone();
    drop(f.engine);
    let e = preflight::engine::Engine::open(config).unwrap();
    assert_eq!(e.db.pipeline_config(&r).unwrap(), exported);
    e.shutdown().await.unwrap();
}
#[tokio::test]
async fn invalid_config_and_external_edit_are_rejected() {
    let f = Fixture::new();
    let mut definition:preflight::pipeline::PipelineFile=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"test","command":f.request(&[],5).command}]}})).unwrap();
    let mut request = SavePipeline {
        project_id: f.project.id.clone(),
        name: "../escape".into(),
        expected_revision: 0,
        definition: definition.clone(),
    };
    assert!(f.engine.db.save_pipeline(&request).is_err());
    request.name = "safe".into();
    f.engine.db.save_pipeline(&request).unwrap();
    let path = f.project.root.join(".preflight/pipelines/safe/1.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["definition"]["pipeline"]["stages"][0]["command"]["program"] = json!("changed");
    std::fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
    request.expected_revision = 1;
    assert!(f.engine.db.save_pipeline(&request).is_err());
    definition.pipeline.stages[0].needs = vec!["missing".into()];
    assert!(
        f.engine
            .start_pipeline(StartPipeline {
                project_id: f.project.id.clone(),
                definition: Some(definition)
            })
            .is_err()
    );
    let r = f.start(&[], 5);
    finished(&f.engine, &r.id).await;
    assert!(!output(&f.engine, &r.id, "stdout").is_empty());
    f.engine.shutdown().await.unwrap();
}
