#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{control, experiments as ex, model::RunState};
use serde_json::json;
#[tokio::test]
async fn experiments_pin_revisions_compare_cases_and_keep_separate_idempotent_history() {
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = common::Fixture::new();
    for (name, result) in [
        ("pass.xml", ""),
        ("fail.xml", "<failure message=\"changed behavior\"/>"),
    ] {
        std::fs::write(
            project.root.join(name),
            format!("<testsuite><testcase name=\"same-test\">{result}</testcase></testsuite>"),
        )
        .unwrap();
    }
    let pipeline = json!({"version":1,"pipeline":{"stages":[{"id":"test","kind":"test","command":{"program":"sh","args":["-c","cp pass.xml report.xml"],"timeout_seconds":5},"junit":["report.xml"]}]}});
    e.db.save_pipeline(&serde_json::from_value(json!({"project_id":project.id,"name":"base","expected_revision":0,"definition":pipeline})).unwrap()).unwrap();
    let baseline = control::start_saved(
        &e,
        control::ConfigRef {
            project_id: project.id.clone(),
            name: "base".into(),
            revision: Some(1),
        },
    )
    .unwrap();
    assert_eq!(
        common::finished(&e, &baseline.id).await.status,
        RunState::Succeeded
    );
    let mut definition =
        json!({"version":1,"pipeline":{"name":"base","revision":1},"baseline_run":baseline.id});
    ex::save(&e,&serde_json::from_value(json!({"project_id":project.id,"name":"control","expected_revision":0,"definition":definition})).unwrap()).unwrap();
    let mut failing = pipeline.clone();
    failing["pipeline"]["stages"][0]["command"]["args"] = json!(["-c", "cp fail.xml report.xml"]);
    definition["override_definition"] = failing;
    ex::save(&e,&serde_json::from_value(json!({"project_id":project.id,"name":"candidate","expected_revision":0,"definition":definition})).unwrap()).unwrap();
    let start = |name: &str, key: &str| {
        serde_json::from_value::<ex::Start>(
            json!({"project_id":project.id,"name":name,"revision":1,"idempotency_key":key}),
        )
        .unwrap()
    };
    let control = ex::start(&e, start("control", "control-one")).unwrap();
    let candidate = ex::start(&e, start("candidate", "candidate-one")).unwrap();
    assert_eq!(
        common::finished(&e, control["run_id"].as_str().unwrap())
            .await
            .status,
        RunState::Succeeded
    );
    assert_eq!(
        common::finished(&e, candidate["run_id"].as_str().unwrap())
            .await
            .status,
        RunState::Failed
    );
    assert_eq!(
        ex::start(&e, start("candidate", "candidate-one")).unwrap(),
        candidate
    );
    let compare = ex::Compare {
        project_id: project.id.clone(),
        run_id: candidate["run_id"].as_str().unwrap().into(),
        baseline_id: None,
        offset: 0,
    };
    let diff = ex::compare(&e, &compare).unwrap();
    assert_eq!(diff["test_changes"][0]["baseline"], "passed");
    assert_eq!(diff["test_changes"][0]["current"], "failed");
    assert_eq!(diff["test_evidence_complete"], true);
    let normal =
        e.db.query_runs(&control::RunQuery {
            project_id: Some(project.id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(normal["runs"].as_array().unwrap().len(), 1);
    assert_eq!(normal["runs"][0]["run_id"], baseline.id);
    let experiment =
        e.db.query_runs(&control::RunQuery {
            project_id: Some(project.id.clone()),
            experiment_name: Some("candidate".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(experiment["runs"].as_array().unwrap().len(), 1);
    definition["override_definition"] = pipeline;
    ex::save(&e,&serde_json::from_value(json!({"project_id":project.id,"name":"candidate","expected_revision":1,"definition":definition})).unwrap()).unwrap();
    let old = ex::get(
        &e,
        &control::ConfigRef {
            project_id: project.id.clone(),
            name: "candidate".into(),
            revision: Some(1),
        },
    )
    .unwrap();
    assert_eq!(
        old["definition"]["override_definition"]["pipeline"]["stages"][0]["command"]["args"][1],
        "cp fail.xml report.xml"
    );
    let config = e.config.clone();
    drop(e);
    let e = preflight::engine::Engine::open(config).unwrap();
    assert_eq!(
        ex::start(&e, start("candidate", "candidate-one")).unwrap(),
        candidate
    );
    assert_eq!(
        ex::compare(&e, &compare).unwrap()["test_changes"],
        diff["test_changes"]
    );
    let foreign = ex::Run {
        project_id: uuid::Uuid::now_v7().to_string(),
        run_id: compare.run_id,
    };
    assert!(ex::status(&e, &foreign).is_err());
    drop(e);
    drop(temp);
}

#[tokio::test]
async fn git_patch_experiments_compare_tests_without_mutating_checkout() {
    let f = common::Fixture::new();
    let original = "<testsuite><testcase name=\"same\"/></testsuite>\n";
    let changed =
        "<testsuite><testcase name=\"same\"><failure message=\"patch\"/></testcase></testsuite>\n";
    std::fs::write(f.project.root.join("source.xml"), original).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Preflight test"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["add", "source.xml"]);
    git(&["commit", "-qm", "base"]);
    let commit = git(&["rev-parse", "HEAD"]).trim().to_string();
    let pipeline = json!({"version":1,"pipeline":{"stages":[{"id":"test","kind":"test","command":{"program":"sh","args":["-c","cp source.xml result.xml"]},"junit":["result.xml"]}]}});
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":"base","expected_revision":0,"definition":pipeline})).unwrap()).unwrap();
    let mut baseline = None;
    let mut runs = Vec::new();
    for name in ["control", "patch"] {
        let patches = if name == "control" {
            json!([])
        } else {
            json!([{"name":"test-change","diff":format!("diff --git a/source.xml b/source.xml\n--- a/source.xml\n+++ b/source.xml\n@@ -1 +1 @@\n-{}+{}",original,changed)}])
        };
        ex::save(&f.engine,&serde_json::from_value(json!({"project_id":f.project.id,"name":name,"expected_revision":0,"definition":{"version":1,"pipeline":{"name":"base","revision":1},"baseline_run":baseline,"source":{"commit":commit,"patches":patches}}})).unwrap()).unwrap();
        let result = ex::start(
            &f.engine,
            serde_json::from_value(
                json!({"project_id":f.project.id,"name":name,"revision":1,"idempotency_key":name}),
            )
            .unwrap(),
        )
        .unwrap();
        let run = result["run_id"].as_str().unwrap().to_string();
        common::finished(&f.engine, &run).await;
        baseline = Some(run.clone());
        runs.push(run);
    }
    assert_eq!(
        f.engine.db.run(&runs[0]).unwrap().status,
        RunState::Succeeded
    );
    assert_eq!(f.engine.db.run(&runs[1]).unwrap().status, RunState::Failed);
    let comparison = ex::compare(
        &f.engine,
        &ex::Compare {
            project_id: f.project.id.clone(),
            run_id: runs[1].clone(),
            baseline_id: None,
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(comparison["test_changes"][0]["baseline"], "passed");
    assert_eq!(comparison["test_changes"][0]["current"], "failed");
    assert_eq!(
        std::fs::read_to_string(f.project.root.join("source.xml")).unwrap(),
        original
    );
    f.engine.shutdown().await.unwrap();
}
