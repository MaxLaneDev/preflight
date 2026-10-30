mod common;
use common::*;
use preflight::{
    inputs::{CompareQuery, InputQuery},
    model::RunState,
};
#[tokio::test]
async fn manifests_preserve_inputs_and_distinguish_configuration() {
    let f = Fixture::new();
    std::fs::write(f.project.root.join("input.txt"), "before").unwrap();
    let a = f.start(&["write"], 5);
    assert_eq!(finished(&f.engine, &a.id).await.status, RunState::Succeeded);
    assert!(output(&f.engine, &a.id, "stdout").contains("Preflight helper"));
    let before = f.engine.db.inputs(&a.id).unwrap().unwrap();
    let key_a = f.engine.db.stage_key(&a.id, "native").unwrap().unwrap();
    assert!(!key_a.reusable);
    assert!(f.engine.db.stage_key(&a.id, "missing").is_err());

    assert!(!before.entries.contains_key("result.txt"));
    std::fs::write(f.project.root.join("input.txt"), "after").unwrap();
    std::fs::write(f.project.root.join("added.txt"), "new").unwrap();
    let b = f.start(&["write"], 5);
    finished(&f.engine, &b.id).await;
    let diff = f
        .engine
        .db
        .compare_inputs(&CompareQuery {
            run_id: b.id.clone(),
            baseline_id: a.id.clone(),
            offset: 0,
        })
        .unwrap();
    assert_eq!(diff["diff"]["total_changes"], 2);
    assert_eq!(diff["diff"]["configuration_changed"], false);
    assert_eq!(
        f.engine.db.inputs(&a.id).unwrap().unwrap().tree_sha256,
        before.tree_sha256
    );
    let evidence = preflight::stage_keys::explain(
        &f.engine.db,
        &preflight::stage_keys::Query {
            run_id: b.id.clone(),
            stage_id: "native".into(),
            baseline_id: Some(a.id.clone()),
        },
    )
    .unwrap();
    assert_eq!(
        evidence["changed_components"],
        serde_json::json!(["source_tree"])
    );
    let unchanged = f.start(&["write"], 5);
    finished(&f.engine, &unchanged.id).await;
    assert_eq!(
        f.engine
            .db
            .stage_key(&unchanged.id, "native")
            .unwrap()
            .unwrap()
            .sha256,
        f.engine
            .db
            .stage_key(&b.id, "native")
            .unwrap()
            .unwrap()
            .sha256
    );
    let c = f.start(&[], 5);
    finished(&f.engine, &c.id).await;
    let diff = f
        .engine
        .db
        .compare_inputs(&CompareQuery {
            run_id: c.id,
            baseline_id: b.id.clone(),
            offset: 0,
        })
        .unwrap();
    assert_eq!(diff["diff"]["tree_changed"], false);
    assert_eq!(diff["diff"]["configuration_changed"], true);
    assert!(
        f.engine
            .db
            .input_page(&InputQuery {
                run_id: b.id,
                offset: 20_001
            })
            .is_err()
    );
}
#[tokio::test]
async fn comparisons_reject_other_projects_and_survive_restart() {
    let f = Fixture::new();
    let a = f.start(&[], 5);
    finished(&f.engine, &a.id).await;
    let path = f.temp.path().join("other");
    std::fs::create_dir(&path).unwrap();
    let other = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "other".into(),
            path: path.to_string_lossy().into_owned(),
        })
        .unwrap();
    let mut request = f.request(&[], 5);
    request.project_id = other.id;
    let b = f.engine.start(request).unwrap();
    finished(&f.engine, &b.id).await;
    assert!(
        f.engine
            .db
            .compare_inputs(&CompareQuery {
                run_id: a.id.clone(),
                baseline_id: b.id.clone(),
                offset: 0
            })
            .is_err()
    );
    assert!(
        preflight::stage_keys::explain(
            &f.engine.db,
            &preflight::stage_keys::Query {
                run_id: a.id.clone(),
                stage_id: "native".into(),
                baseline_id: Some(b.id),
            }
        )
        .is_err()
    );
    let key = f
        .engine
        .db
        .stage_key(&a.id, "native")
        .unwrap()
        .unwrap()
        .sha256;
    let hash = f.engine.db.inputs(&a.id).unwrap().unwrap().tree_sha256;
    let db_path = f.engine.config.data_dir.join("preflight.sqlite3");
    let reopened = preflight::db::Db::new(&db_path).unwrap();
    assert_eq!(reopened.inputs(&a.id).unwrap().unwrap().tree_sha256, hash);
    assert_eq!(
        reopened.stage_key(&a.id, "native").unwrap().unwrap().sha256,
        key
    );
}
