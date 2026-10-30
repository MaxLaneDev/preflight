#[allow(dead_code)]
mod common;
use common::{Fixture, finished};
use preflight::{
    model::RunState,
    pipeline::{self, PipelineFile, StartPipeline},
};
use serde_json::json;
fn pipeline(stages: serde_json::Value) -> PipelineFile {
    serde_json::from_value(json!({"version":1,"pipeline":{"stages":stages}})).unwrap()
}
#[tokio::test]
async fn setup_installs_missing_workspace_tool_and_keeps_environment_for_main() {
    let fixture = Fixture::new();
    std::fs::write(std::path::Path::new(&fixture.project.root).join("prepare.sh"),"mkdir -p tools\nprintf '#!/bin/sh\\nprintf installed-tool\\n' > tools/fresh-tool\nchmod +x tools/fresh-tool\nexport PATH=\"$PREFLIGHT_WORKSPACE/tools:$PATH\"\n").unwrap();
    let definition = pipeline(
        json!([{"id":"test","setup":[{"program":".","args":["./prepare.sh"]}],"command":{"program":"fresh-tool"}}]),
    );
    let run = fixture
        .engine
        .start_pipeline(StartPipeline {
            project_id: fixture.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert_eq!(
        finished(&fixture.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let logs = fixture
        .engine
        .db
        .stage_logs(&run.id, Some("test"), 0)
        .unwrap();
    assert!(
        serde_json::to_string(&logs)
            .unwrap()
            .contains("installed-tool")
    );
}
#[tokio::test]
async fn setup_failure_blocks_main_and_timeout_cancels_all_setup_commands() {
    let fixture = Fixture::new();
    let definition = pipeline(
        json!([{"id":"failed","setup":[{"program":"sh","args":["-c","exit 7"]}],"command":{"program":"sh","args":["-c","echo main-should-not-run"]}}]),
    );
    let run = fixture
        .engine
        .start_pipeline(StartPipeline {
            project_id: fixture.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert_eq!(
        finished(&fixture.engine, &run.id).await.status,
        RunState::Failed
    );
    let logs = serde_json::to_string(
        &fixture
            .engine
            .db
            .stage_logs(&run.id, Some("failed"), 0)
            .unwrap(),
    )
    .unwrap();
    assert!(logs.contains("setup step 1 failed"));
    assert!(!logs.contains("main-should-not-run"));
    let definition = pipeline(
        json!([{"id":"timeout","setup":[{"program":"sh","args":["-c","sleep 30"]}],"command":{"program":"sh","args":["-c","echo main-should-not-run"],"timeout_seconds":1}}]),
    );
    let run = fixture
        .engine
        .start_pipeline(StartPipeline {
            project_id: fixture.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert_eq!(
        finished(&fixture.engine, &run.id).await.status,
        RunState::TimedOut
    );
}
#[test]
fn setup_and_image_references_have_strict_contracts() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let valid = pipeline(
        json!([{"id":"prepare","command":{"program":"docker","args":["build","--iidfile","image-id","."]},"outputs":{"image":{"path":"image-id","type":"file"}}},{"id":"test","needs":["prepare"],"executor":"docker","container":{"image_from":"${artifact.prepare.image}","platform":"linux/arm64"},"setup":[{"program":"sh","args":["-c","true"]}],"command":{"program":"cc"}}]),
    );
    pipeline::validate(&valid).unwrap();
    let mut bad = valid.clone();
    bad.pipeline.stages[1].needs.clear();
    assert!(pipeline::validate(&bad).is_err());
    let mut bad = valid.clone();
    bad.pipeline.stages[1].container.as_mut().unwrap().image = digest;
    assert!(pipeline::validate(&bad).is_err());
    let mut bad = valid.clone();
    bad.pipeline.stages[0]
        .outputs
        .get_mut("image")
        .unwrap()
        .kind = pipeline::ArtifactKind::Directory;
    assert!(pipeline::validate(&bad).is_err());
    let mut bad = valid;
    bad.pipeline.stages[1].setup[0].args = vec!["${artifact.prepare.image}".into()];
    assert!(pipeline::validate(&bad).is_err());
    assert!(
        serde_json::from_value::<pipeline::SetupCommand>(
            json!({"program":"sh","timeout_seconds":2})
        )
        .is_err()
    );
}
