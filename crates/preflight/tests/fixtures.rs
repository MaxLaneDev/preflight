use preflight::{
    config::Config,
    engine::Engine,
    fixtures::{self, Comparator, FixtureAction, Numeric, StartBatch},
    model::{Actor, CancelRun, RegisterProject, Run, RunState},
};
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};

fn copy(source: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for e in std::fs::read_dir(source).unwrap() {
        let e = e.unwrap();
        if e.path().is_dir() {
            copy(&e.path(), &dest.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), dest.join(e.file_name())).unwrap();
        }
    }
}
fn setup() -> (tempfile::TempDir, Arc<Engine>, StartBatch, Config) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    copy(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/fixture-compiler"
        )),
        &root,
    );
    let config = Config {
        release_repository: None,
        ssh_command: None,
        worker_launcher: None,
        worker_bind: None,
        memory_budget_mib: None,
        min_free_disk_bytes: None,
        max_storage_mib: None,
        admin_token_file: None,
        docker_command: None,
        access_tokens: Vec::new(),
        ci_sources: Vec::new(),
        default_stage_memory_mib: 256,
        codex_command: None,
        version: 1,
        storage_policies: vec![],
        max_active_runs: 32,
        max_parallel_commands: 10,
        bind: "127.0.0.1:4170".parse().unwrap(),
        data_dir: temp.path().join("data"),
        allowed_roots: vec![root.clone()],
    };
    let e = Engine::open(config.clone()).unwrap();
    let p = e
        .register(RegisterProject {
            name: "fixtures".into(),
            path: root.to_string_lossy().into(),
        })
        .unwrap();
    (
        temp,
        e,
        StartBatch {
            project_id: p.id,
            path: "preflight-fixtures.yaml".into(),
        },
        config,
    )
}
async fn finished(e: &Engine, id: &str) -> Run {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let r = e.db.run(id).unwrap();
            if r.status.terminal() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
fn single(e: &Engine, request: &StartBatch) -> preflight::pipeline::PipelineFile {
    let mut p = e.configured_batch(request).unwrap();
    p.pipeline.stages.truncate(2);
    p.pipeline.stages[1]
        .fixtures
        .as_mut()
        .unwrap()
        .suite
        .cases
        .truncate(1);
    p
}
#[test]
fn comparators_are_explicit_and_preserve_baselines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("golden.txt"), "hello\n").unwrap();
    for (c, actual, passed) in [
        (Comparator::Exact("hello".into()), "hello\n", false),
        (Comparator::Trim("hello".into()), "hello\n", true),
        (Comparator::Contains("ell".into()), "hello", true),
        (Comparator::Regex("^h[a-z]+$".into()), "hello", true),
        (
            Comparator::Json(json!({"a":[1,2],"b":true})),
            "{\"b\":true,\"a\":[1,2]}",
            true,
        ),
        (Comparator::Json(json!([1, 2])), "[2,1]", false),
        (
            Comparator::Numeric(Numeric {
                value: 1.,
                tolerance: 0.01,
            }),
            "1.005",
            true,
        ),
        (
            Comparator::Numeric(Numeric {
                value: 1.,
                tolerance: 0.01,
            }),
            "NaN",
            false,
        ),
        (
            Comparator::Sha256(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            ),
            "",
            true,
        ),
        (Comparator::Snapshot("golden.txt".into()), "hello\n", true),
        (Comparator::Snapshot("golden.txt".into()), "goodbye", false),
    ] {
        let r = fixtures::compare("stdout", &c, actual.as_bytes(), dir.path()).unwrap();
        assert_eq!(r.passed, passed, "{c:?}");
        if !passed {
            assert!(!r.diff.is_empty());
        }
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("golden.txt")).unwrap(),
        "hello\n"
    );
}
#[test]
fn schema_rejects_bad_paths_templates_regex_and_modes() {
    let (_temp, e, request, _) = setup();
    let p = e.configured_batch(&request).unwrap();
    let a = p.pipeline.stages[1].fixtures.clone().unwrap();
    fixtures::validate(&a).unwrap();
    let base = serde_json::to_value(&a).unwrap();
    let mut variants = vec![];
    let mut v = base.clone();
    v["suite"]["cases"][0]["files"] = json!({"../main.my":"bad"});
    variants.push(v);
    let mut v = base.clone();
    v["adapter"]["compile"]["args"] = json!(["prefix{entry}"]);
    variants.push(v);
    let mut v = base.clone();
    v["adapter"]["diagnostics_regex"] = json!("[");
    variants.push(v);
    let mut v = base.clone();
    v["suite"]["cases"][0]["expect"]["run"] = json!(null);
    variants.push(v);
    let mut v = base.clone();
    v["suite"]["cases"][1]["id"] = json!("echo");
    variants.push(v);
    let mut v = base.clone();
    v["suite"]["cases"][0]["expect"]["run"]["stdout"] =
        json!({"numeric":{"value":1,"tolerance":-1}});
    variants.push(v);
    for v in variants {
        let invalid = serde_json::from_value::<FixtureAction>(v).unwrap();
        assert!(fixtures::validate(&invalid).is_err());
    }
    let mut v = base;
    v["unknown"] = json!(true);
    assert!(serde_json::from_value::<FixtureAction>(v).is_err());
}
#[tokio::test]
async fn ten_groups_share_one_build_diff_cleanup_and_durable_comparison() {
    let (temp, e, request, config) = setup();
    let mut p = e.configured_batch(&request).unwrap();
    let first = p.pipeline.stages[1].fixtures.as_mut().unwrap();
    let mut extra = first.suite.cases[1].clone();
    extra.id = "extra-negative".into();
    first.suite.cases.push(extra);

    let last = p
        .pipeline
        .stages
        .last_mut()
        .unwrap()
        .fixtures
        .as_mut()
        .unwrap();
    last.suite.cases[0].expect.run.as_mut().unwrap().stdout =
        Some(Comparator::Exact("wrong expectation".into()));
    let run = e
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: request.project_id.clone(),
            definition: Some(p),
        })
        .unwrap();
    let run = finished(&e, &run.id).await;
    assert_eq!(run.status, RunState::Failed, "{run:#?}");
    assert_eq!(run.stages.len(), 11);
    assert_eq!(run.stages[0].status, RunState::Succeeded);
    assert_eq!(e.db.artifacts(&run.id).unwrap().len(), 1);
    let groups = &run.stages[1..];
    assert!(
        groups.iter().map(|s| s.started_at.unwrap()).max().unwrap()
            < groups.iter().map(|s| s.finished_at.unwrap()).min().unwrap(),
        "all ten groups overlapped"
    );
    let report = e.db.fixture_results(&run.id, 0).unwrap();
    assert_eq!(report["counts"]["succeeded"], 20);
    assert_eq!(report["next_cursor"], 20);
    assert_eq!(
        e.db.fixture_results(&run.id, 20).unwrap()["cases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(report["counts"]["failed"], 1);
    let diff =
        e.db.fixture_diff(&run.id, "fixtures-group-10", "echo")
            .unwrap();
    let check = diff["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["target"] == "run.stdout")
        .unwrap();
    assert_eq!(check["expected"], "wrong expectation");
    assert_eq!(check["actual"], "group-10 input");
    assert!(!check["diff"].as_str().unwrap().is_empty());
    for stage in groups {
        assert_eq!(
            std::fs::read_dir(
                run.workspace
                    .join("stages")
                    .join(&stage.definition.id)
                    .join(".preflight/cases")
            )
            .unwrap()
            .count(),
            0
        );
    }
    let good = e.start_batch(request).unwrap();
    let good = finished(&e, &good.id).await;
    assert_eq!(good.status, RunState::Succeeded);
    let comparison = e.db.compare_batches(&good.id, &run.id).unwrap();
    assert_eq!(comparison["changed"], 2); // Fixed failure and removed extra case.
    e.shutdown().await.unwrap();
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(
        e.db.fixture_diff(&run.id, "fixtures-group-10", "echo")
            .unwrap(),
        diff
    );
    assert!(temp.path().join("project/suites/group-10.yaml").exists());
    e.shutdown().await.unwrap();
}
#[tokio::test]
async fn fixture_timeout_and_cancel_clean_temporary_directories() {
    let (_temp, e, request, _) = setup();
    for cancel in [false, true] {
        let mut p = single(&e, &request);
        let a = p.pipeline.stages[1].fixtures.as_mut().unwrap();
        a.adapter.timeout_seconds = if cancel { 30 } else { 1 };
        a.suite.cases[0]
            .files
            .insert("main.my".into(), "print SLEEP".into());
        let r = e
            .start_pipeline(preflight::pipeline::StartPipeline {
                project_id: request.project_id.clone(),
                definition: Some(p),
            })
            .unwrap();
        if cancel {
            tokio::time::timeout(Duration::from_secs(30), async {
                while e
                    .db
                    .fixture_diff(&r.id, "fixtures-group-01", "echo")
                    .unwrap()["status"]
                    != "running"
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            e.cancel(CancelRun {
                run_id: r.id.clone(),
                actor: Actor::User,
                reason: "cancel fixture".into(),
            })
            .unwrap();
        }
        let r = finished(&e, &r.id).await;
        assert_eq!(
            r.status,
            if cancel {
                RunState::Cancelled
            } else {
                RunState::TimedOut
            }
        );
        assert_eq!(
            std::fs::read_dir(
                r.workspace
                    .join("stages/fixtures-group-01/.preflight/cases")
            )
            .unwrap()
            .count(),
            0
        );
    }
    e.shutdown().await.unwrap();
}

#[test]
fn mcp_diff_is_bounded_and_prioritizes_failures() {
    let long = "é".repeat(10000);
    let full = json!({"batch_id":"r","stage_id":"s","case_id":"c","checks":[
        {"passed":true,"expected":"ok","actual":"ok"},
        {"passed":false,"expected":long,"actual":long},
        {"passed":true,"expected":"ok","actual":"ok"},
        {"passed":true,"expected":"ok","actual":"ok"}]});
    let compact = fixtures::compact_diff(full.clone());
    assert_eq!(compact["truncated"], true);
    assert_eq!(compact["checks"][0]["passed"], false);
    assert_eq!(
        compact["checks"][0]["actual"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        2048
    );
    assert_eq!(
        full["checks"][1]["actual"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        10000
    );
}
#[tokio::test]
async fn truncated_output_missing_baseline_and_failed_build_are_explicit() {
    let (temp, e, request, _) = setup();
    let source = temp.path().join("project/compiler.rs");
    std::fs::write(&source, "fn main(){print!(\"{}\",\"x\".repeat(70000));}").unwrap();
    let mut p = single(&e, &request);
    let a = p.pipeline.stages[1].fixtures.as_mut().unwrap();
    a.suite.cases[0].execute.mode = fixtures::Mode::Compile;
    a.suite.cases[0].expect.run = None;
    a.suite.cases[0].expect.files.clear();
    let r = e
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: request.project_id.clone(),
            definition: Some(p.clone()),
        })
        .unwrap();
    let r = finished(&e, &r.id).await;
    assert_eq!(r.status, RunState::Failed);
    let d =
        e.db.fixture_diff(&r.id, "fixtures-group-01", "echo")
            .unwrap();
    assert!(
        d["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["target"] == "compile.output" && c["passed"] == false)
    );
    std::fs::write(&source, "fn main(){print!(\"hello\");}").unwrap();
    p.pipeline.stages[1].fixtures.as_mut().unwrap().suite.cases[0]
        .expect
        .compile
        .stdout = Some(Comparator::Snapshot("absent.txt".into()));
    let r = e
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: request.project_id.clone(),
            definition: Some(p.clone()),
        })
        .unwrap();
    let r = finished(&e, &r.id).await;
    assert_eq!(r.status, RunState::InfrastructureError);
    std::fs::write(&source, "this is invalid Rust").unwrap();
    let r = e
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: request.project_id,
            definition: Some(p),
        })
        .unwrap();
    let r = finished(&e, &r.id).await;
    assert_eq!(r.status, RunState::Failed);
    assert_eq!(
        e.db.fixture_results(&r.id, 0).unwrap()["counts"]["blocked"],
        1
    );
    e.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_fixture_feed_and_legacy_archived_logs_keep_case_scope() {
    let (_temp, e, request, config) = setup();
    let run = e.start_batch(request).unwrap();
    let run = finished(&e, &run.id).await;
    assert_eq!(run.status, RunState::Succeeded);
    let stage = run
        .stages
        .iter()
        .find(|s| s.definition.fixtures.is_some())
        .unwrap();
    let case = &stage.definition.fixtures.as_ref().unwrap().suite.cases[0].id;
    let expected: usize = run
        .stages
        .iter()
        .filter_map(|s| s.definition.fixtures.as_ref())
        .map(|a| a.suite.cases.len())
        .sum();
    let feed = preflight::inspection::tests(
        &e,
        &preflight::inspection::Tests {
            project_id: run.project_id.clone(),
            run_id: run.id.clone(),
            offset: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(feed["total"], expected);
    assert!(
        feed["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["native_fixture"] == true)
    );
    rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE log_events SET test_id=NULL WHERE run_id=?1",
            [&run.id],
        )
        .unwrap();
    e.db.logs_archive(
        &serde_json::from_value(
            json!({"project_id":run.project_id,"run_id":run.id,"max_chunks":16}),
        )
        .unwrap(),
    )
    .unwrap();
    let query=serde_json::from_value(json!({"project_id":run.project_id,"run_id":run.id,"stage_id":stage.definition.id,"test_id":case})).unwrap();
    let found = preflight::evidence::search(&e, &query).unwrap();
    assert!(!found["matches"].as_array().unwrap().is_empty());
    assert!(
        found["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["test_id"] == *case && c["stage_id"] == stage.definition.id)
    );
}
