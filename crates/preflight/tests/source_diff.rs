#[allow(dead_code)]
mod common;
use common::*;
use preflight::{
    model::RunState,
    source_diff::{self, Query},
};
#[tokio::test]
async fn compares_captured_bytes_and_rejects_missing_modified_or_foreign_evidence() {
    let f = Fixture::new();
    std::fs::write(f.project.root.join("text"), "before\n").unwrap();
    std::fs::write(f.project.root.join("deleted"), "gone\n").unwrap();
    std::fs::write(f.project.root.join("binary"), [0, 1, 2]).unwrap();
    let a = f.start(&[], 5);
    assert_eq!(finished(&f.engine, &a.id).await.status, RunState::Succeeded);
    std::fs::write(f.project.root.join("text"), "after\n").unwrap();
    std::fs::remove_file(f.project.root.join("deleted")).unwrap();
    std::fs::write(f.project.root.join("added"), "new\n").unwrap();
    let b = f.start(&[], 5);
    assert_eq!(finished(&f.engine, &b.id).await.status, RunState::Succeeded);
    let query = |path: &str| Query {
        project_id: f.project.id.clone(),
        run_id: b.id.clone(),
        baseline_id: a.id.clone(),
        path: path.into(),
    };
    std::fs::write(f.project.root.join("text"), "unrelated live edit").unwrap();
    let changed = source_diff::compare(f.engine.clone(), query("text"))
        .await
        .unwrap();
    assert!(
        changed["diff"]
            .as_str()
            .unwrap()
            .contains("-before\n+after\n")
    );
    assert_eq!(changed["verified"], true);
    assert_eq!(
        source_diff::compare(f.engine.clone(), query("added"))
            .await
            .unwrap()["before_exists"],
        false
    );
    assert_eq!(
        source_diff::compare(f.engine.clone(), query("deleted"))
            .await
            .unwrap()["after_exists"],
        false
    );
    for path in ["binary", "missing", "../text"] {
        assert!(
            source_diff::compare(f.engine.clone(), query(path))
                .await
                .is_err()
        );
    }
    let mut foreign = query("text");
    foreign.project_id = uuid::Uuid::now_v7().to_string();
    assert!(
        source_diff::compare(f.engine.clone(), foreign)
            .await
            .is_err()
    );
    std::fs::write(b.workspace.join("text"), "tampered\n").unwrap();
    assert!(
        source_diff::compare(f.engine.clone(), query("text"))
            .await
            .is_err()
    );
    std::fs::remove_file(a.workspace.join("deleted")).unwrap();
    assert!(
        source_diff::compare(f.engine.clone(), query("deleted"))
            .await
            .is_err()
    );
}
