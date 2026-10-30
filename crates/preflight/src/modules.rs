use crate::{
    artifacts,
    engine::Engine,
    model::{Artifact, invalid},
    pipeline,
};
use anyhow::{Result, ensure};
use fs2::FileExt;
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    pub run_id: String,
    pub stage_id: String,
    pub artifact_name: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Get {
    pub project_id: String,
    pub sha256: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestQuery {
    pub project_id: String,
    pub sha256: String,
    #[serde(default)]
    pub offset: usize,
}
/// Native contracts retain their original digest; containers cannot publish as legacy.
pub(crate) fn execution_contract(
    db: &crate::db::Db,
    run: &str,
    stage: Option<&crate::pipeline::StageDefinition>,
) -> Result<Option<(String, serde_json::Value)>> {
    let verified = db.toolchain(run)?;
    let mut resolved = stage.cloned();
    if let Some(stage) = &mut resolved
        && stage.container.is_some()
    {
        anyhow::ensure!(
            stage.worker.is_none(),
            "remote container module publication requires a verified remote runtime contract"
        );
        anyhow::ensure!(
            stage.setup.is_empty(),
            "module contracts require a prepared immutable image, not mutable setup commands"
        );
        crate::docker::resolve_image(db, run, stage)?;
    }
    let Some(container) = resolved.as_ref().and_then(|s| s.container.as_ref()) else {
        return Ok(verified.map(|v| (v.contract_sha256, v.contract)));
    };
    let contract = serde_json::json!({"format":"preflight-container-module-v1","executor":crate::docker::RUNTIME_FORMAT,"container":container,"prepared_profile":verified.map(|v|v.contract)});
    let sha = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&contract)?));
    Ok(Some((sha, contract)))
}
pub fn reference(value: &str) -> Result<Option<&str>> {
    if !value.contains("${module") {
        return Ok(None);
    }
    let digest = value
        .strip_prefix("${module.")
        .and_then(|v| v.strip_suffix('}'))
        .ok_or_else(|| invalid("module reference must occupy whole value"))?;
    validate_digest(digest)?;
    Ok(Some(digest))
}
pub fn validate_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        invalid("module SHA-256 must be 64 lowercase hexadecimal characters")
    );
    Ok(())
}
pub fn publish(e: &Engine, request: &Publish) -> Result<Artifact> {
    let run = e.db.run(&request.run_id)?;
    ensure!(
        run.status == crate::model::RunState::Succeeded,
        invalid("module producer run must have succeeded")
    );
    publish_stage(e, request)
}
pub(crate) fn publish_stage(e: &Engine, request: &Publish) -> Result<Artifact> {
    let run = e.db.run(&request.run_id)?;
    ensure!(
        run.status.terminal()
            && run
                .stages
                .iter()
                .any(|s| s.definition.id == request.stage_id
                    && s.status == crate::model::RunState::Succeeded),
        invalid("module producer stage must have succeeded in a terminal run")
    );
    let mut artifact =
        e.db.artifacts(&run.id)?
            .into_iter()
            .find(|a| a.stage_id == request.stage_id && a.name == request.artifact_name)
            .ok_or(crate::model::Fault::NotFound)?;
    let _operation = e.db.begin_operation(
        &run.id,
        "module_publish",
        std::collections::BTreeSet::from([format!("module:{}", artifact.sha256)]),
    )?;
    e.db.require_artifact_available(&run.id, &request.stage_id)?;
    let root = pipeline::contained(
        &e.config.data_dir,
        &format!("modules/{}/{}", run.project_id, artifact.sha256),
    )?;
    std::fs::create_dir_all(&root)?;
    let temporary = tempfile::tempdir_in(&root)?;
    let checked = temporary.path().join("verified");
    let manifest = artifacts::verify_copy(&artifact, &checked)?;
    let destination = pipeline::contained(&root, "content")?;
    // Serializes publication of this digest across callers and processes.
    let lock_path = pipeline::contained(&root, "publication.lock")?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    if !destination.exists() {
        std::fs::rename(&checked, &destination)?;
    }
    artifact.path = destination;
    artifacts::verify_copy(&artifact, &temporary.path().join("rechecked"))?;
    e.db.publish_module(&run.project_id, &artifact, manifest.as_ref())?;
    e.db.module(&run.project_id, &artifact.sha256)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn container_contracts_pin_image_platform_policy_and_preserve_native_legacy() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(&root.path().join("test.sqlite3")).unwrap();
        let mut stage:crate::pipeline::StageDefinition=serde_json::from_value(serde_json::json!({"id":"build","executor":"docker","container":{"image":format!("sha256:{}","a".repeat(64)),"platform":"linux/arm64"},"command":{"program":"cc"}})).unwrap();
        let project = db.register("test", root.path()).unwrap();
        db.insert_run(
            "unused",
            &crate::model::StartBuild {
                project_id: project.id,
                command: stage.command.clone(),
            },
            root.path(),
        )
        .unwrap();
        let first = execution_contract(&db, "unused", Some(&stage))
            .unwrap()
            .unwrap();
        assert_eq!(first.1["format"], "preflight-container-module-v1");
        assert!(execution_contract(&db, "unused", None).unwrap().is_none());
        stage.container.as_mut().unwrap().platform = "linux/amd64".into();
        assert_ne!(
            execution_contract(&db, "unused", Some(&stage))
                .unwrap()
                .unwrap()
                .0,
            first.0
        );
        stage.container.as_mut().unwrap().platform = "linux/arm64".into();
        stage.container.as_mut().unwrap().image = format!("sha256:{}", "b".repeat(64));
        assert_ne!(
            execution_contract(&db, "unused", Some(&stage))
                .unwrap()
                .unwrap()
                .0,
            first.0
        );
    }
}
