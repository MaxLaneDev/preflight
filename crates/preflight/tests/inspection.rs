#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{inspection, model::RunState};
use serde_json::json;
#[tokio::test]
async fn individual_cases_source_snapshot_and_pipeline_history_remain_run_scoped() {
    let f = common::Fixture::new();
    std::fs::create_dir(f.project.root.join("src")).unwrap();
    std::fs::write(f.project.root.join("src/test.rs"), "fn test() {}\n").unwrap();
    let xml = r#"<testsuite name="suite"><testcase name="same" classname="module" file="src/test.rs" line="1" time="0.12"/><testcase name="bad"><failure message="mismatch"/></testcase><testcase name="later"><skipped/></testcase></testsuite>"#;
    std::fs::write(f.project.root.join("report-template.xml"), xml).unwrap();
    let definition = json!({"version":1,"pipeline":{"stages":[{"id":"tests","kind":"test","command":{"program":"sh","args":["-c","cp report-template.xml report.xml"],"timeout_seconds":5},"junit":["report.xml"]}]}});
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value(json!({"project_id":f.project.id,"definition":definition}))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        common::finished(&f.engine, &run.id).await.status,
        RunState::Failed
    );
    let query = inspection::Tests {
        project_id: f.project.id.clone(),
        run_id: run.id.clone(),
        offset: 0,
        ..Default::default()
    };
    let results = inspection::tests(&f.engine, &query).unwrap();
    assert_eq!(results["total"], 3);
    assert_eq!(results["stages"][0]["cases_complete"], true);
    assert_eq!(results["cases"][0]["case"]["status"], "passed");
    assert_eq!(results["cases"][1]["case"]["status"], "failed");
    assert_eq!(results["cases"][2]["case"]["status"], "skipped");
    let source = inspection::Source {
        project_id: f.project.id.clone(),
        run_id: run.id.clone(),
        stage_id: "tests".into(),
        test_id: results["cases"][0]["case"]["id"].as_str().unwrap().into(),
    };
    let viewed = inspection::source(&f.engine, &source).unwrap();
    assert_eq!(viewed["original_matches"], true);
    assert_eq!(viewed["code"], "fn test() {}\n");
    std::fs::write(f.project.root.join("src/test.rs"), "changed\n").unwrap();
    let viewed = inspection::source(&f.engine, &source).unwrap();
    assert_eq!(viewed["opens_snapshot"], true);
    assert_eq!(viewed["code"], "fn test() {}\n");
    let groups = inspection::pipelines(
        &f.engine,
        &inspection::Pipelines {
            project_id: f.project.id.clone(),
        },
    )
    .unwrap();
    let key = groups["pipelines"][0]["pipeline_key"].as_str().unwrap();
    let page = f
        .engine
        .db
        .query_runs(&preflight::control::RunQuery {
            project_id: Some(f.project.id.clone()),
            pipeline_key: Some(key.into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page["runs"][0]["run_id"], run.id);
    let foreign = inspection::Tests {
        project_id: uuid::Uuid::now_v7().to_string(),
        ..query
    };
    assert!(inspection::tests(&f.engine, &foreign).is_err());
}
#[test]
fn summary_does_not_invent_cases_and_legacy_results_deserialize() {
    let summary = preflight::junit::parse(r#"<testsuite tests="3" failures="0"/>"#).unwrap();
    assert_eq!(summary.counts.passed, 3);
    assert!(summary.cases.is_empty());
    assert!(!summary.cases_complete);
    let legacy: preflight::model::TestReport = serde_json::from_value(
        json!({"counts":{"passed":2,"failed":0,"skipped":0},"failures":[],"truncated":false}),
    )
    .unwrap();
    assert!(!legacy.cases_complete);
    assert!(legacy.cases.is_empty());
    let duplicate = preflight::junit::parse(
        r#"<testsuite><testcase name="a"/><testcase name="a"/></testsuite>"#,
    )
    .unwrap();
    assert_ne!(duplicate.cases[0].id, duplicate.cases[1].id);
}

#[tokio::test]
async fn test_search_filters_before_pagination_and_preserves_observed_counts() {
    let f = common::Fixture::new();
    let run = f.start(&["echo", "search-fixture"], 5);
    common::finished(&f.engine, &run.id).await;
    let cases=(0..260).map(|n|serde_json::json!({"id":format!("test-id-{n}"),"name":format!("Parser::case-{n:03}"),"class_name":"Parser","suite":"UnitGroup","status":"passed","message":"","file":null,"line":null,"duration_seconds":null})).collect::<Vec<_>>();
    let report = serde_json::json!({"cases":cases,"cases_complete":true,"counts":{"passed":260,"failed":0,"skipped":0},"failures":[],"truncated":false});
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute(
        "UPDATE stage_runs SET tests_json=?1 WHERE run_id=?2",
        rusqlite::params![report.to_string(), run.id],
    )
    .unwrap();
    for term in [" case-259 ", "TEST-ID-259"] {
        let q = inspection::Tests {
            project_id: f.project.id.clone(),
            run_id: run.id.clone(),
            query: Some(term.into()),
            ..Default::default()
        };
        let result = inspection::tests(&f.engine, &q).unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["cases"][0]["case"]["id"], "test-id-259");
        assert!(result["next_cursor"].is_null());
        assert_eq!(result["stages"][0]["counts"]["passed"], 260);
    }
    let q = inspection::Tests {
        project_id: f.project.id.clone(),
        run_id: run.id.clone(),
        query: Some("unitgroup".into()),
        offset: 200,
        ..Default::default()
    };
    let page = inspection::tests(&f.engine, &q).unwrap();
    assert_eq!(page["total"], 260);
    assert_eq!(page["cases"].as_array().unwrap().len(), 60);
    let q = inspection::Tests {
        status: Some("failed".into()),
        ..q
    };
    assert_eq!(inspection::tests(&f.engine, &q).unwrap()["total"], 0);
}
