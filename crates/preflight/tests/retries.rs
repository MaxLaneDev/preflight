#![cfg(unix)]
#[allow(dead_code)]
mod common;
use common::*;
use preflight::{
    model::*,
    pipeline::{PipelineFile, StartPipeline},
    retry::{self, Selection, Start},
};
use serde_json::json;
fn selection(f: &Fixture, run: &Run) -> Selection {
    Selection {
        project_id: f.project.id.clone(),
        run_id: run.id.clone(),
        stage_id: "unit-a".into(),
        test_id: None,
        stage_override: None,
        source: None,
    }
}
fn request(s: Selection, key: &str) -> Start {
    Start {
        selection: s,
        idempotency_key: key.into(),
        actor: Actor::Agent,
        reason: "corrected test source".into(),
    }
}
async fn parent(f: &Fixture) -> Run {
    let counter = f.temp.path().join("build-count");
    let runner = "#!/bin/sh\nname=$1\nstate=passed\nif [ \"$(cat value.txt)\" != ok ] && [ \"$name\" = case-a ]; then state=failed; fi\nprintf '{\"version\":1,\"cases\":[{\"name\":\"%s\",\"status\":\"%s\",\"suite\":\"unit\"}]}' \"$name\" \"$state\" > report.json\n";
    std::fs::write(f.project.root.join("runner.sh"), runner).unwrap();
    std::fs::write(f.project.root.join("value.txt"), "bad").unwrap();
    let command =
        json!({"program":"${artifact.build.runner}","args":["${test.name}"],"timeout_seconds":5});
    let definition:PipelineFile=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[
 {"id":"build","command":{"program":"sh","args":["-c",format!("printf x >> '{}'; cp runner.sh built-runner; chmod +x built-runner",counter.display())]},"outputs":{"runner":{"path":"built-runner","type":"executable"}}},
 {"id":"unit-a","kind":"test","needs":["build"],"command":{"program":"${artifact.build.runner}","args":["case-a"]},"testing":{"kind":"unit","reports":[{"format":"preflight_json","path":"report.json"}],"rerun":command}},
 {"id":"unit-b","kind":"test","needs":["build"],"command":{"program":"${artifact.build.runner}","args":["case-b"]},"testing":{"kind":"unit","reports":[{"format":"preflight_json","path":"report.json"}]}}
 ]}})).unwrap();
    let run = f
        .engine
        .start_pipeline(StartPipeline {
            project_id: f.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert!(retry::preview(&f.engine, &selection(f, &run)).is_err());
    let run = finished(&f.engine, &run.id).await;
    assert_eq!(run.status, RunState::Failed);
    assert_eq!(run.stages[0].status, RunState::Succeeded);
    assert_eq!(run.stages[2].status, RunState::Succeeded);
    run
}
#[tokio::test]
async fn corrected_single_test_reuses_failed_parent_build_and_preserves_history() {
    let f = Fixture::new();
    let old = parent(&f).await;
    let original = serde_json::to_value(&old).unwrap();
    let mut s = selection(&f, &old);
    s.test_id = Some(old.stages[1].tests.as_ref().unwrap().cases[0].id.clone());
    let preview = retry::preview(&f.engine, &s).unwrap();
    assert_eq!(
        preview["retry"]["reused_artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(preview["stage"]["command"]["args"][0], "case-a");
    std::fs::write(f.project.root.join("value.txt"), "ok").unwrap();
    let new = retry::start(&f.engine, request(s.clone(), "fixed-case")).unwrap();
    let done = finished(&f.engine, &new.id).await;
    assert_eq!(done.status, RunState::Succeeded);
    assert_eq!(done.stages.len(), 1);
    assert_eq!(done.stages[0].tests.as_ref().unwrap().counts.passed, 1);
    assert_eq!(done.retry.as_ref().unwrap()["parent_run_id"], old.id);
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("build-count")).unwrap(),
        "x"
    );
    assert_eq!(
        serde_json::to_value(f.engine.db.run(&old.id).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        retry::start(&f.engine, request(s.clone(), "fixed-case"))
            .unwrap()
            .id,
        new.id
    );
    let mut changed = s.clone();
    changed.test_id = None;
    assert!(retry::start(&f.engine, request(changed, "fixed-case")).is_err());
    let report = preflight::api::compact_report(&f.engine, &new.id).unwrap();
    assert_eq!(report["retry"]["whole_pipeline_verdict"], false);
    assert!(
        report["submission"]["request"]["selection"]
            .get("stage_override")
            .is_none()
    );
    let mut again = selection(&f, &done);
    again.test_id = s.test_id.clone();
    let repeated = retry::start(&f.engine, request(again, "second-attempt")).unwrap();
    assert_eq!(
        finished(&f.engine, &repeated.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("build-count")).unwrap(),
        "x"
    );
    f.engine.shutdown().await.unwrap();
    let config = f.engine.config.clone();
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(config).unwrap();
    assert_eq!(reopened.db.run(&new.id).unwrap().retry, done.retry);
}
#[tokio::test]
async fn stage_retry_and_validation_fail_closed() {
    let f = Fixture::new();
    let old = parent(&f).await;
    let s = selection(&f, &old);
    let mut wrong = s.clone();
    wrong.project_id = "other-project".into();
    assert!(retry::preview(&f.engine, &wrong).is_err());
    assert!(retry::start(&f.engine, request(wrong.clone(), "wrong-project")).is_err());
    wrong = s.clone();
    wrong.stage_id = "missing".into();
    assert!(retry::preview(&f.engine, &wrong).is_err());
    wrong = s.clone();
    wrong.test_id = Some("missing".into());
    assert!(retry::preview(&f.engine, &wrong).is_err());
    let mut override_stage = old.stages[1].definition.clone();
    override_stage.needs.push("unit-a".into());
    wrong = s.clone();
    wrong.stage_override = Some(override_stage);
    assert!(retry::preview(&f.engine, &wrong).is_err());
    std::fs::write(f.project.root.join("value.txt"), "ok").unwrap();
    let new = retry::start(&f.engine, request(s.clone(), "stage-only")).unwrap();
    assert_eq!(
        finished(&f.engine, &new.id).await.status,
        RunState::Succeeded
    );
    let artifact = f.engine.db.artifacts(&old.id).unwrap().remove(0);
    std::fs::set_permissions(
        &artifact.path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(artifact.path, "tampered").unwrap();
    assert!(retry::start(&f.engine, request(s, "tampered")).is_err());
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn single_test_rejects_runner_that_reports_a_different_case() {
    let f = Fixture::new();
    let old = parent(&f).await;
    let mut s = selection(&f, &old);
    s.test_id = Some(old.stages[1].tests.as_ref().unwrap().cases[0].id.clone());
    let mut stage = old.stages[1].definition.clone();
    stage.testing.as_mut().unwrap().rerun.as_mut().unwrap().args =
        vec!["case-b".into(), "${test.name}".into()];
    s.stage_override = Some(stage);
    let new = retry::start(&f.engine, request(s, "wrong-case")).unwrap();
    let done = finished(&f.engine, &new.id).await;
    assert_eq!(done.status, RunState::InfrastructureError);
    assert!(
        done.stages[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("selected test")
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn edited_fixture_case_runs_alone_with_existing_compiler() {
    let f = Fixture::new();
    let source = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/fixture-compiler"
    ));
    for name in ["compiler.rs", "preflight-fixtures.yaml"] {
        std::fs::copy(source.join(name), f.project.root.join(name)).unwrap();
    }
    std::fs::create_dir(f.project.root.join("suites")).unwrap();
    for entry in std::fs::read_dir(source.join("suites")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            f.project.root.join("suites").join(entry.file_name()),
        )
        .unwrap();
    }
    let path = f.project.root.join("suites/group-01.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("group-01 input", "wrong expectation");
    std::fs::write(path, text).unwrap();
    let old = f
        .engine
        .start_batch(preflight::fixtures::StartBatch {
            project_id: f.project.id.clone(),
            path: "preflight-fixtures.yaml".into(),
        })
        .unwrap();
    let old = finished(&f.engine, &old.id).await;
    assert_eq!(old.status, RunState::Failed);
    let original = old
        .stages
        .iter()
        .find(|s| {
            s.definition
                .fixtures
                .as_ref()
                .is_some_and(|a| a.suite.suite == "group-01")
        })
        .unwrap();
    let mut stage = original.definition.clone();
    let case = stage
        .fixtures
        .as_mut()
        .unwrap()
        .suite
        .cases
        .iter_mut()
        .find(|c| c.id == "echo")
        .unwrap();
    case.files.insert("main.my".into(), "print fixed".into());
    case.expect.run.as_mut().unwrap().stdout =
        Some(preflight::fixtures::Comparator::Exact("fixed input".into()));
    case.expect.files.insert(
        "result.txt".into(),
        preflight::fixtures::Comparator::Exact("fixed".into()),
    );
    let s = Selection {
        project_id: f.project.id.clone(),
        run_id: old.id.clone(),
        stage_id: stage.id.clone(),
        test_id: Some("echo".into()),
        stage_override: Some(stage),
        source: None,
    };
    let preview = retry::preview(&f.engine, &s).unwrap();
    assert_eq!(preview["stage"]["fixture_cases"], json!(["echo"]));
    let new = retry::start(&f.engine, request(s, "edited-fixture")).unwrap();
    let done = finished(&f.engine, &new.id).await;
    assert_eq!(done.status, RunState::Succeeded);
    assert_eq!(done.stages.len(), 1);
    assert_eq!(done.stages[0].tests.as_ref().unwrap().counts.passed, 1);
    assert_eq!(f.engine.db.run(&old.id).unwrap().status, RunState::Failed);
    f.engine.shutdown().await.unwrap();
}
