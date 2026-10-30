#[allow(dead_code)]
mod common;
use preflight::log_search::{self, Query};
#[tokio::test]
async fn search_filters_projects_stages_pages_and_redacts_credential_shapes() {
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    f.engine
        .db
        .append_stage_log(
            &run.id,
            "native",
            "stdout",
            100,
            "compile error alpha token=supersecret\n",
        )
        .unwrap();
    f.engine
        .db
        .append_stage_log(
            &run.id,
            "native",
            "stderr",
            101,
            "compile error beta Authorization: Bearer abcdef\n",
        )
        .unwrap();
    let q = Query {
        project_id: f.project.id.clone(),
        query: "compile error".into(),
        run_id: Some(run.id.clone()),
        stage_id: Some("native".into()),
        before: None,
        limit: 1,
    };
    let first = log_search::search(&f.engine, &q).unwrap();
    assert_eq!(first["matches"].as_array().unwrap().len(), 1);
    assert!(!first.to_string().contains("abcdef"));
    assert!(first.to_string().contains("REDACTED"));
    let second = log_search::search(
        &f.engine,
        &Query {
            before: first["next_cursor"].as_i64(),
            ..q
        },
    )
    .unwrap();
    assert!(!second.to_string().contains("supersecret"));
    assert_ne!(
        first["matches"][0]["evidence_id"],
        second["matches"][0]["evidence_id"]
    );
    let foreign = Query {
        project_id: uuid::Uuid::now_v7().to_string(),
        query: "compile".into(),
        run_id: Some(run.id),
        stage_id: None,
        before: None,
        limit: 20,
    };
    assert!(log_search::search(&f.engine, &foreign).is_err());
}
