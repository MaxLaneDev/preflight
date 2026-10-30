use crate::{
    control::{self, ConfigRef, SaveDefinition},
    engine::Engine,
    model::*,
    pipeline::{
        self, PipelineDefinition, PipelineFile, ProjectMetadata, StageDefinition, StartPipeline,
    },
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(default, skip_serializing_if = "is_false")]
    pub run_in_place: bool,
    pub program: String,
    #[serde(default)]
    pub args_prefix: Vec<String>,
    pub probe_args: Option<Vec<String>>,
}
fn is_false(value: &bool) -> bool {
    !*value
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub tools: BTreeMap<String, Tool>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub host_os: Option<String>,
    pub host_arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pins: Option<crate::pins::Pins>,
}
pub type SaveProfile = SaveDefinition<Profile>;
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub name: String,
    pub revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SavedStart {
    pub project_id: String,
    pub name: String,
    pub revision: Option<u64>,
    pub profile: Option<Revision>,
    pub idempotency_key: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub project_id: String,
    pub name: String,
    pub revision: u64,
    pub idempotency_key: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Submission {
    pub key: Option<String>,
    pub fingerprint: String,
    pub request: Value,
}
impl Submission {
    pub(crate) fn new(key: Option<String>, request: Value) -> Result<Self> {
        if let Some(k) = &key {
            ensure!(
                !k.is_empty()
                    && k.len() <= 128
                    && k.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b)),
                invalid("idempotency key must be 1..128 printable identifier characters")
            );
        }
        Ok(Self {
            key,
            fingerprint: crate::hashing::hex(Sha256::digest(serde_json::to_vec(&request)?)),
            request,
        })
    }
}
pub fn validate(profile: &Profile) -> Result<()> {
    ensure!(profile.version == 1, invalid("unsupported profile version"));
    ensure!(
        (1..=16).contains(&profile.tools.len()),
        invalid("profile requires 1..16 tools")
    );
    ensure!(
        serde_json::to_vec(profile)?.len() <= 65536,
        invalid("profile exceeds 64 KiB")
    );
    for value in profile.host_os.iter().chain(profile.host_arch.iter()) {
        ensure!(
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            invalid("invalid host constraint")
        );
    }
    ensure!(
        profile.pins.is_some() || profile.tools.values().all(|t| !t.run_in_place),
        invalid("run_in_place requires an explicit content pin")
    );
    crate::pins::validate(profile)?;
    let mut stages = vec![];
    for (name, tool) in &profile.tools {
        control::validate_name(name)?;
        ensure!(
            !tool.program.contains(['{', '}']),
            invalid("profile tool program cannot contain placeholders")
        );
        let command = CommandSpec {
            program: tool.program.clone(),
            args: tool.args_prefix.clone(),
            timeout_seconds: 30,
        };
        pipeline::validate_command(&command)?;
        if let Some(probe) = &tool.probe_args {
            let mut command = command.clone();
            command.args.extend(probe.iter().cloned());
            pipeline::validate_command(&command)?;
        }
        let mut stage = StageDefinition::native(command);
        stage.id = name.clone();
        stage.env = profile.env.clone();
        stages.push(stage);
    }
    pipeline::validate(&PipelineFile {
        version: 1,
        project: None,
        pipeline: PipelineDefinition {
            acceptance: vec![],
            max_parallel_stages: 2,
            stages,
        },
    })
}
pub fn get(e: &Engine, r: &ConfigRef) -> Result<Value> {
    control::get_document::<Profile>(
        &e.db.project(&r.project_id)?.root,
        "toolchains",
        r,
        validate,
    )
}
pub fn save(e: &Engine, r: &SaveProfile) -> Result<Value> {
    control::save_document(
        &e.db.project(&r.project_id)?.root,
        "toolchains",
        r,
        validate,
    )
}
pub fn list(e: &Engine, id: &str) -> Result<Value> {
    control::list_documents(&e.db.project(id)?.root, "toolchains", id)
}
pub fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Profile)).expect("schema serializes")
}
fn host(profile: &Profile) -> Result<()> {
    ensure!(
        profile
            .host_os
            .as_deref()
            .is_none_or(|s| s == std::env::consts::OS),
        invalid("profile host OS does not match this worker")
    );
    ensure!(
        profile
            .host_arch
            .as_deref()
            .is_none_or(|s| s == std::env::consts::ARCH),
        invalid("profile host architecture does not match this worker")
    );
    Ok(())
}
pub(crate) fn apply(file: &mut PipelineFile, profile: &Profile) -> Result<()> {
    host(profile)?;
    for stage in &mut file.pipeline.stages {
        if stage.command.program.contains("${tool") {
            let alias = stage
                .command
                .program
                .strip_prefix("${tool.")
                .and_then(|s| s.strip_suffix('}'))
                .ok_or_else(|| invalid("tool reference must be a whole program value"))?;
            let tool = profile
                .tools
                .get(alias)
                .ok_or_else(|| invalid("unknown tool alias"))?;
            stage.command.program = if profile.pins.is_some() {
                format!("${{pinned.{alias}}}")
            } else {
                tool.program.clone()
            };
            let mut args = tool.args_prefix.clone();
            args.extend(stage.command.args.clone());
            stage.command.args = args;
        }
        if profile.pins.is_some() {
            ensure!(
                stage.setup.is_empty(),
                invalid(
                    "pinned profiles require already prepared tools; use a prepared image instead of setup"
                )
            );
            ensure!(
                stage.fixtures.is_none(),
                invalid("pinned profiles currently require native stages")
            );
            ensure!(
                stage.command.program.starts_with("${pinned.")
                    || pipeline::artifact_ref(&stage.command.program)?.is_some()
                    || crate::modules::reference(&stage.command.program)?.is_some(),
                invalid("pinned stages must use declared tool, artifact or module programs")
            );
        }
        let mut env = profile.env.clone();
        env.extend(stage.env.clone());
        stage.env = env;
    }
    pipeline::validate(file)
}
pub fn start(e: &Arc<Engine>, r: SavedStart) -> Result<Run> {
    ensure!(
        r.revision.is_some_and(|v| v > 0),
        invalid("saved start requires explicit positive pipeline revision")
    );
    let mut submission = Submission::new(
        r.idempotency_key.clone(),
        json!({"operation":"pipeline_run_saved","request":r}),
    )?;
    if let Some(run) = e.db.submitted(&r.project_id, &submission)? {
        return Ok(run);
    }
    let saved = e.db.pipeline_config(&ConfigRef {
        project_id: r.project_id.clone(),
        name: r.name,
        revision: r.revision,
    })?;
    let mut file: PipelineFile = serde_json::from_value(saved["definition"].clone())?;
    submission.request["pipeline_sha256"] = saved["sha256"].clone();
    if let Some(p) = r.profile {
        ensure!(p.revision > 0, invalid("profile revision must be positive"));
        let saved = get(
            e,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: p.name,
                revision: Some(p.revision),
            },
        )?;
        let profile: Profile = serde_json::from_value(saved["definition"].clone())?;
        apply(&mut file, &profile)?;
        submission.request["profile_snapshot"] = serde_json::to_value(&profile)?;
        submission.request["profile_sha256"] = saved["sha256"].clone();
    }
    e.start_pipeline_submission(
        StartPipeline {
            project_id: r.project_id,
            definition: Some(file),
        },
        Some(submission),
    )
}
pub fn probe(e: &Arc<Engine>, r: Probe) -> Result<Run> {
    ensure!(r.revision > 0, invalid("profile revision must be positive"));
    let mut submission = Submission::new(
        r.idempotency_key.clone(),
        json!({"operation":"toolchain_probe","request":r}),
    )?;
    if let Some(run) = e.db.submitted(&r.project_id, &submission)? {
        return Ok(run);
    }
    let saved = get(
        e,
        &ConfigRef {
            project_id: r.project_id.clone(),
            name: r.name.clone(),
            revision: Some(r.revision),
        },
    )?;
    let profile: Profile = serde_json::from_value(saved["definition"].clone())?;
    host(&profile)?;
    let mut stages = vec![];
    for (name, tool) in &profile.tools {
        if let Some(probe) = &tool.probe_args {
            let mut args = tool.args_prefix.clone();
            args.extend(probe.iter().cloned());
            let mut stage = StageDefinition::native(CommandSpec {
                program: if profile.pins.is_some() {
                    format!("${{pinned.{name}}}")
                } else {
                    tool.program.clone()
                },
                args,
                timeout_seconds: 30,
            });
            stage.id = name.clone();
            stage.env = profile.env.clone();
            stages.push(stage);
        }
    }
    ensure!(
        !stages.is_empty(),
        invalid("profile has no configured probes")
    );
    submission.request["profile_sha256"] = saved["sha256"].clone();
    submission.request["profile_snapshot"] = serde_json::to_value(&profile)?;
    let file = PipelineFile {
        version: 1,
        project: Some(ProjectMetadata {
            name: format!("Probe {}", r.name),
            source: Default::default(),
        }),
        pipeline: PipelineDefinition {
            acceptance: vec![],
            stages,
            max_parallel_stages: 2,
        },
    };
    e.start_pipeline_submission(
        StartPipeline {
            project_id: r.project_id,
            definition: Some(file),
        },
        Some(submission),
    )
}

pub fn start_pinned(e: &Arc<Engine>, r: crate::pins::StartPinned) -> Result<Run> {
    ensure!(
        r.profile.revision > 0,
        invalid("profile revision must be positive")
    );
    let mut submission = Submission::new(
        r.idempotency_key.clone(),
        json!({"operation":"pipeline_start_pinned","request":r}),
    )?;
    if let Some(run) = e.db.submitted(&r.project_id, &submission)? {
        return Ok(run);
    }
    if let Some(source) = &r.source {
        source.identity()?;
    }
    let saved = get(
        e,
        &ConfigRef {
            project_id: r.project_id.clone(),
            name: r.profile.name,
            revision: Some(r.profile.revision),
        },
    )?;
    let profile: Profile = serde_json::from_value(saved["definition"].clone())?;
    ensure!(
        profile.pins.is_some(),
        invalid("pinned start requires profile pins")
    );
    let mut file = r.definition;
    apply(&mut file, &profile)?;
    submission.request["profile_sha256"] = saved["sha256"].clone();
    submission.request["profile_snapshot"] = serde_json::to_value(&profile)?;
    e.start_pipeline_source(
        StartPipeline {
            project_id: r.project_id,
            definition: Some(file),
        },
        Some(submission),
        r.source,
    )
}
