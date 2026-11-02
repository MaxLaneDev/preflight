use crate::model::{CommandSpec, invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PipelineFile {
    pub version: u32,
    #[serde(default)]
    pub project: Option<ProjectMetadata>,
    pub pipeline: PipelineDefinition,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectMetadata {
    pub name: String,
    #[serde(default)]
    pub source: Source,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Local,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PipelineDefinition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<crate::acceptance::Criterion>,
    #[serde(default = "default_parallel")]
    pub max_parallel_stages: usize,
    pub stages: Vec<StageDefinition>,
}
fn default_parallel() -> usize {
    2
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing: Option<crate::testing::Configuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<crate::remote_stage::Selector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<crate::cache::Policy>,
    pub id: String,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub kind: StageKind,
    #[serde(default)]
    pub executor: Executor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<crate::docker::Spec>,
    /// Literal POSIX commands run in order before command, in the same execution environment.
    /// The command timeout covers setup and execution together.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<SetupCommand>,
    pub command: CommandSpec,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Destination paths relative to the stage root, mapped to whole artifact references.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputSpec>,
    /// Explicit JUnit paths relative to cwd; no globs or inferred report counts.
    #[serde(default)]
    pub junit: Vec<String>,
    #[serde(default)]
    pub fixtures: Option<crate::fixtures::FixtureAction>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequirements {
    #[serde(default = "default_slots")]
    pub slots: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
}
fn default_slots() -> u32 {
    1
}
fn default_cwd() -> String {
    ".".into()
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    #[default]
    Build,
    Test,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    #[default]
    Native,
    Docker,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Directory,
    File,
    Executable,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: ArtifactKind,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartPipeline {
    pub project_id: String,
    /// Omit to load the registered project's preflight.yaml.
    pub definition: Option<PipelineFile>,
}
impl StageDefinition {
    pub fn report_paths(&self) -> impl Iterator<Item = &String> {
        self.junit.iter().chain(
            self.testing
                .iter()
                .flat_map(|t| t.reports.iter().map(|r| &r.path)),
        )
    }
    pub fn required_slots(&self) -> u32 {
        self.resources.as_ref().map_or(1, |r| r.slots)
    }

    pub fn native(command: CommandSpec) -> Self {
        Self {
            testing: None,
            worker: None,
            resources: None,
            cache: None,
            id: "native".into(),
            needs: vec![],
            kind: StageKind::Build,
            executor: Executor::Native,
            container: None,
            setup: vec![],
            command,
            cwd: ".".into(),
            env: BTreeMap::new(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            junit: vec![],
            fixtures: None,
        }
    }
}
pub fn parse(yaml: &str) -> Result<PipelineFile> {
    ensure!(
        yaml.len() <= 64 * 1024,
        invalid("pipeline YAML exceeds 64 KiB")
    );
    let file: PipelineFile =
        serde_saphyr::from_str(yaml).map_err(|e| invalid(format!("invalid pipeline YAML: {e}")))?;
    validate(&file)?;
    Ok(file)
}
pub fn validate(file: &PipelineFile) -> Result<()> {
    ensure!(file.version == 1, invalid("unsupported pipeline version"));
    if let Some(project) = &file.project {
        ensure!(
            !project.name.is_empty() && project.name.len() <= 128,
            invalid("project name must be 1..128 bytes")
        );
    }
    let plan = &file.pipeline;
    ensure!(
        (1..=32).contains(&plan.stages.len()),
        invalid("pipeline must have 1..32 stages")
    );
    ensure!(
        (1..=32).contains(&plan.max_parallel_stages),
        invalid("max_parallel_stages must be 1..32")
    );
    let mut ids = BTreeMap::new();
    for stage in &plan.stages {
        identifier(&stage.id)?;
        ensure!(
            matches!(stage.executor, Executor::Docker) == stage.container.is_some(),
            invalid(
                "Docker executor requires container settings; native stages must not have container settings"
            )
        );
        if let Some(container) = &stage.container {
            container.validate()?;
            if stage.worker.is_some() {
                ensure!(
                    stage.cache.is_none(),
                    invalid("remote container stages do not support cache reuse")
                );
            }
        }

        if let Some(worker) = &stage.worker {
            crate::remote_stage::validate(worker)?;
            ensure!(
                stage.fixtures.is_none() && stage.cache.is_none(),
                invalid(
                    "remote stages currently require ordinary commands without fixture adapters or cache reuse"
                )
            );
        }
        ensure!(
            (1..=32).contains(&stage.required_slots()),
            invalid("stage resource slots must be 1..32")
        );
        if let Some(policy) = &stage.cache {
            identifier(&policy.pipeline_id)?;
            ensure!(
                policy.deterministic && policy.inputs_complete,
                invalid("cache requires explicit deterministic and inputs_complete declarations")
            );
            ensure!(
                stage.kind == StageKind::Build
                    && stage.fixtures.is_none()
                    && stage.junit.is_empty()
                    && !stage.outputs.is_empty(),
                invalid(
                    "cache supports build stages with declared outputs and no test reports or fixtures"
                )
            );
        }
        ensure!(
            ids.insert(stage.id.as_str(), stage).is_none(),
            invalid("duplicate stage id")
        );
        validate_stage_command(&stage.command, stage.container.is_some())?;
        ensure!(
            stage.setup.len() <= 16,
            invalid("at most 16 setup commands per stage")
        );
        if !stage.setup.is_empty() {
            ensure!(
                stage.fixtures.is_none() && stage.cache.is_none(),
                invalid("setup requires ordinary commands without fixture adapters or cache reuse")
            );
            ensure!(
                !(matches!(stage.executor, Executor::Native)
                    && stage
                        .worker
                        .as_ref()
                        .map(|w| w.os.as_str())
                        .unwrap_or(std::env::consts::OS)
                        == "windows"),
                invalid("setup requires a POSIX sh execution environment")
            );
            for setup in &stage.setup {
                validate_stage_command(
                    &CommandSpec {
                        program: setup.program.clone(),
                        args: setup.args.clone(),
                        timeout_seconds: stage.command.timeout_seconds,
                    },
                    stage.container.is_some(),
                )?;
                ensure!(
                    !std::iter::once(&setup.program).chain(&setup.args).any(|v| [
                        "${artifact",
                        "${module",
                        "${tool",
                        "${pinned",
                        "${sdk"
                    ]
                    .iter()
                    .any(|marker| v.contains(marker))),
                    invalid("setup uses literal argv; materialize dependencies with stage inputs")
                );
            }
        }
        crate::testing::validate(stage)?;
        if let Some(action) = &stage.fixtures {
            crate::fixtures::validate(action)?;
            ensure!(
                stage.kind == StageKind::Test
                    && stage.junit.is_empty()
                    && stage.outputs.is_empty()
                    && stage.command.args.is_empty(),
                invalid("fixture stages require kind test, no args/JUnit/outputs")
            );
        }
        relative(&stage.cwd)?;
        ensure!(
            stage.needs.len() <= 32
                && stage.needs.iter().collect::<BTreeSet<_>>().len() == stage.needs.len(),
            invalid("duplicate or excessive needs")
        );
        ensure!(
            stage.outputs.len() <= 16 && stage.inputs.len() <= 32 && stage.junit.len() <= 16,
            invalid("too many stage files")
        );
        ensure!(
            stage.kind == StageKind::Test || stage.junit.is_empty(),
            invalid("JUnit reports belong to test stages")
        );
        let mut paths = BTreeSet::new();
        for (name, output) in &stage.outputs {
            identifier(name)?;
            let path = relative(&output.path)?;
            ensure!(
                !path.as_os_str().is_empty() && paths.insert(path),
                invalid("empty or colliding output path")
            );
        }
        for a in &paths {
            for b in &paths {
                ensure!(
                    a == b || !a.starts_with(b),
                    invalid("overlapping output paths")
                );
            }
        }
        let mut inputs = BTreeSet::new();
        for path in stage.inputs.keys() {
            let path = relative(path)?;
            ensure!(
                !path.as_os_str().is_empty() && inputs.insert(path),
                invalid("empty or colliding input path")
            );
        }
        for a in &inputs {
            for b in &inputs {
                ensure!(
                    a == b || !a.starts_with(b),
                    invalid("overlapping input paths")
                );
            }
        }
        let mut junit_paths = BTreeSet::new();
        for path in &stage.junit {
            ensure!(
                junit_paths.insert(relative(path)?),
                invalid("duplicate JUnit report path")
            );
            ensure!(
                !relative(path)?.as_os_str().is_empty(),
                invalid("empty JUnit path")
            );
        }
        ensure!(
            stage.junit.iter().collect::<BTreeSet<_>>().len() == stage.junit.len(),
            invalid("duplicate JUnit report")
        );
        ensure!(
            stage.env.len() <= 32,
            invalid("too many environment values")
        );
        for (key, value) in &stage.env {
            ensure!(
                !key.is_empty()
                    && key.len() <= 128
                    && key.bytes().enumerate().all(|(i, b)| b == b'_'
                        || b.is_ascii_alphabetic()
                        || (i > 0 && b.is_ascii_digit())),
                invalid("invalid environment key")
            );
            ensure!(
                !key.eq_ignore_ascii_case("PREFLIGHT_WORKSPACE")
                    && value.len() <= 8192
                    && !value.contains('\0')
                    && !value.contains("${artifact")
                    && !value.contains("${module"),
                invalid("invalid/reserved environment value")
            );
        }
    }
    for stage in &plan.stages {
        for dep in &stage.needs {
            ensure!(
                ids.contains_key(dep.as_str()) && dep != &stage.id,
                invalid("missing or self dependency")
            );
        }
        for value in stage.inputs.values() {
            ensure!(
                artifact_ref(value)?.is_some() || crate::modules::reference(value)?.is_some(),
                invalid("inputs require an artifact reference")
            );
        }
        for (value, program) in std::iter::once((&stage.command.program, true))
            .chain(stage.command.args.iter().map(|s| (s, false)))
            .chain(stage.inputs.values().map(|s| (s, false)))
            .chain(
                stage
                    .container
                    .iter()
                    .filter_map(|c| c.image_from.as_ref())
                    .map(|s| (s, false)),
            )
        {
            if crate::modules::reference(value)?.is_some() {
                ensure!(
                    stage
                        .container
                        .as_ref()
                        .and_then(|c| c.image_from.as_ref())
                        .is_none(),
                    invalid(
                        "cross-run modules require a literal pinned image; prepared images can consume same-run artifacts"
                    )
                );
            }
            if let Some((producer, name)) = artifact_ref(value)? {
                ensure!(
                    stage.needs.iter().any(|d| d == producer),
                    invalid("artifact producer must be an explicit dependency")
                );
                let output = ids
                    .get(producer)
                    .and_then(|s| s.outputs.get(name))
                    .ok_or_else(|| invalid("unknown artifact reference"))?;
                if stage.container.as_ref().and_then(|c| c.image_from.as_ref()) == Some(value) {
                    if let Some(worker) = &stage.worker {
                        let producer_worker = ids.get(producer).and_then(|s| s.worker.as_ref());
                        ensure!(
                            worker.worker_id.is_some()
                                && producer_worker.is_some_and(|p| p.worker_id == worker.worker_id
                                    && p.os == worker.os
                                    && p.arch == worker.arch),
                            invalid(
                                "remote prepared image requires the same explicit worker for producer and consumer"
                            )
                        );
                    }
                    ensure!(
                        output.kind == ArtifactKind::File,
                        invalid("container image_from requires a file artifact")
                    );
                }
                ensure!(
                    !program || output.kind == ArtifactKind::Executable,
                    invalid("program artifact must have executable type")
                );
            }
        }
    }
    let mut visited = BTreeSet::new();
    loop {
        let before = visited.len();
        for stage in &plan.stages {
            if stage.needs.iter().all(|dep| visited.contains(dep)) {
                visited.insert(stage.id.clone());
            }
        }
        if visited.len() == plan.stages.len() {
            break;
        }
        ensure!(visited.len() > before, invalid("pipeline dependency cycle"));
    }
    ensure!(
        serde_json::to_vec(file)?.len() <= 64 * 1024,
        invalid("expanded pipeline exceeds 64 KiB")
    );
    crate::acceptance::validate(file)?;
    Ok(())
}
pub fn validate_command(command: &CommandSpec) -> Result<()> {
    validate_stage_command(command, false)
}
fn validate_stage_command(command: &CommandSpec, container: bool) -> Result<()> {
    if command.program.contains("${tool") {
        let alias = command
            .program
            .strip_prefix("${tool.")
            .and_then(|v| v.strip_suffix('}'))
            .ok_or_else(|| invalid("tool alias must occupy the whole program"))?;
        identifier(alias)?;
    }
    ensure!(
        !command.program.is_empty()
            && command.program.len() <= 4096
            && !command.program.contains('\0'),
        invalid("invalid executable")
    );
    ensure!(
        command.args.len() <= 128
            && command
                .args
                .iter()
                .all(|s| s.len() <= 8192 && !s.contains('\0')),
        invalid("command arguments exceed limits")
    );
    ensure!(
        (1..=3600).contains(&command.timeout_seconds),
        invalid("timeout_seconds must be 1..3600")
    );
    if artifact_ref(&command.program)?.is_none() && command.program.contains(['/', '\\']) {
        relative(if container {
            command.program.trim_start_matches('/')
        } else {
            &command.program
        })?;
    }
    Ok(())
}
pub(crate) fn identifier(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        invalid("identifiers must be 1..64 ASCII letters, digits, '_' or '-'")
    );
    Ok(())
}
pub fn artifact_ref(value: &str) -> Result<Option<(&str, &str)>> {
    if !value.contains("${artifact") {
        return Ok(None);
    }
    let inner = value
        .strip_prefix("${artifact.")
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| invalid("artifact substitution must occupy the whole argv value"))?;
    let (stage, name) = inner
        .split_once('.')
        .ok_or_else(|| invalid("invalid artifact reference"))?;
    identifier(stage)?;
    identifier(name)?;
    Ok(Some((stage, name)))
}
/// Serialize an internally derived relative path without relaxing wire-path validation.
pub(crate) fn portable_relative(path: &Path) -> Result<String> {
    let parts = path
        .components()
        .map(|part| {
            let std::path::Component::Normal(part) = part else {
                return Err(invalid("invalid relative path"));
            };
            part.to_str()
                .ok_or_else(|| invalid("relative path is not UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    let value = parts.join("/");
    ensure!(!value.is_empty(), invalid("empty relative path"));
    relative(&value)?;
    Ok(value)
}
pub fn relative(value: &str) -> Result<PathBuf> {
    ensure!(
        value.len() <= 4096 && !value.contains(['\\', ':', '\0']),
        invalid("invalid relative path")
    );
    let path = Path::new(value);
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => {
                ensure!(name != ".preflight", invalid(".preflight is reserved"));
                result.push(name)
            }
            _ => return Err(invalid("path must stay inside stage workspace")),
        }
    }
    Ok(result)
}
/// Validate every existing component so outputs and inputs cannot follow a symlink.
pub fn contained(root: &Path, relative_path: &str) -> Result<PathBuf> {
    let relative = relative(relative_path)?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "symlink in stage path: {}",
                path.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn native_relative_paths_serialize_without_weakening_wire_paths() {
        let native = Path::new("licenses").join("crate").join("LICENSE");
        assert_eq!(
            portable_relative(&native).unwrap(),
            "licenses/crate/LICENSE"
        );
        for invalid in ["", "../outside", "/absolute", "a:b"] {
            assert!(portable_relative(Path::new(invalid)).is_err(), "{invalid}");
        }
        assert!(relative(r"a\b").is_err());
        #[cfg(unix)]
        assert!(portable_relative(Path::new(r"a\b")).is_err());
        #[cfg(windows)]
        assert_eq!(portable_relative(Path::new(r"a\b")).unwrap(), "a/b");
    }
}
