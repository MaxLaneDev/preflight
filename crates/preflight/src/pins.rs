//! Explicit content pins and declared compatibility; not a hermetic execution sandbox.
use crate::{
    artifacts,
    config::Config,
    model::{Artifact, Run, invalid},
    pipeline::{ArtifactKind, PipelineFile},
    profiles::Profile,
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
pub const FORMAT: &str = "preflight-toolchain-v1";
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub path: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub tools: BTreeMap<String, Pin>,
    #[serde(default)]
    pub sdks: BTreeMap<String, Pin>,
    pub target: String,
    #[serde(default)]
    pub abi: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub source_path: PathBuf,
    pub artifact: Artifact,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verified {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub in_place_tools: BTreeSet<String>,
    pub format: String,
    pub contract_sha256: String,
    pub contract: Value,
    pub tools: BTreeMap<String, Input>,
    pub sdks: BTreeMap<String, Input>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inspect {
    pub project_id: String,
    pub path: String,
    pub kind: ArtifactKind,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartPinned {
    pub project_id: String,
    pub definition: PipelineFile,
    pub profile: crate::profiles::Revision,
    pub source: Option<crate::sources::SourceRequest>,
    pub idempotency_key: Option<String>,
}
pub fn validate(profile: &Profile) -> Result<()> {
    let Some(pins) = &profile.pins else {
        return Ok(());
    };
    ensure!(
        pins.tools.keys().eq(profile.tools.keys()),
        invalid("every profile tool requires exactly one pin")
    );
    ensure!(
        pins.sdks.len() <= 8 && pins.abi.len() <= 32,
        invalid("too many SDKs or ABI labels")
    );
    ensure!(
        !pins.target.is_empty()
            && pins.target.len() <= 128
            && pins
                .target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
        invalid("invalid target descriptor")
    );
    for (key, value) in &pins.abi {
        crate::control::validate_name(key)?;
        ensure!(
            !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
            invalid("invalid ABI label")
        );
    }
    for (name, pin) in pins.tools.iter().chain(&pins.sdks) {
        crate::control::validate_name(name)?;
        crate::modules::validate_digest(&pin.sha256)?;
        ensure!(
            !pin.path.is_empty() && pin.path.len() <= 4096 && !pin.path.contains('\0'),
            invalid("invalid pin path")
        );
    }
    Ok(())
}
fn resolve(config: &Config, project: &Path, value: &str) -> Result<PathBuf> {
    let path = project
        .join(value)
        .canonicalize()
        .context("resolve pinned input")?;
    ensure!(
        config.allowed_roots.iter().any(|p| path.starts_with(p))
            && !path.starts_with(&config.data_dir)
            && !config.data_dir.starts_with(&path),
        invalid("pinned input is outside allowed roots or inside server data")
    );
    Ok(path)
}
fn copy(
    source: &Path,
    destination: &Path,
    kind: ArtifactKind,
    remaining: &mut u64,
) -> Result<(String, u64)> {
    let (sha, size) = if kind == ArtifactKind::Directory {
        let manifest = crate::trees::copy(source, destination)?;
        (manifest.sha256()?, manifest.size())
    } else {
        artifacts::copy_hashed_bounded(source, destination, kind, *remaining)?
    };
    *remaining = remaining
        .checked_sub(size)
        .context("toolchain inputs exceed 128 MiB total")?;
    Ok((sha, size))
}
pub fn inspect(config: &Config, project: &Path, request: &Inspect) -> Result<Value> {
    let source = resolve(config, project, &request.path)?;
    let temporary = tempfile::tempdir_in(&config.data_dir)?;
    let (sha, size) = copy(
        &source,
        &temporary.path().join("input"),
        request.kind,
        &mut (128 * 1024 * 1024),
    )?;
    Ok(
        json!({"path":source,"kind":request.kind,"sha256":sha,"size":size,"hash_format":if request.kind == ArtifactKind::Directory {crate::trees::FORMAT}else{"sha256-bytes"},"status":"observed; pin is rechecked at run preparation"}),
    )
}
pub fn prepare(
    config: &Config,
    project: &Path,
    run: &Run,
    profile: &Profile,
    cancel: &CancellationToken,
) -> Result<Verified> {
    validate(profile)?;
    let pins = profile.pins.as_ref().context("profile has no pins")?;
    std::fs::create_dir_all(&run.workspace)?;
    let temporary = tempfile::tempdir_in(&run.workspace)?;
    let candidate = temporary.path().join("toolchain");
    std::fs::create_dir(&candidate)?;
    let final_path = run.workspace.join("toolchain");
    let mut remaining = 128 * 1024 * 1024;
    let mut tools = BTreeMap::new();
    let mut sdks = BTreeMap::new();
    for (label, entries, kind, output) in [
        ("tools", &pins.tools, ArtifactKind::Executable, &mut tools),
        ("sdks", &pins.sdks, ArtifactKind::Directory, &mut sdks),
    ] {
        std::fs::create_dir(candidate.join(label))?;
        for (name, pin) in entries {
            ensure!(!cancel.is_cancelled(), "toolchain preparation cancelled");
            let source = resolve(config, project, &pin.path)?;
            let (sha, size) = copy(
                &source,
                &candidate.join(label).join(name),
                kind,
                &mut remaining,
            )?;
            ensure!(
                sha == pin.sha256,
                invalid(format!("pinned {label}.{name} digest mismatch"))
            );
            output.insert(
                name.clone(),
                Input {
                    source_path: source,
                    artifact: Artifact {
                        run_id: run.id.clone(),
                        stage_id: "toolchain".into(),
                        name: name.clone(),
                        kind,
                        sha256: sha,
                        size,
                        path: final_path.join(label).join(name),
                    },
                },
            );
        }
    }
    let in_place_tools = profile
        .tools
        .iter()
        .filter(|(_, t)| t.run_in_place)
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let mut contract = json!({"format":FORMAT,"host_os":std::env::consts::OS,"host_arch":std::env::consts::ARCH,"target":pins.target,"abi":pins.abi,"tools":profile.tools.iter().map(|(name,t)|(name,json!({"sha256":pins.tools[name].sha256,"args_prefix":t.args_prefix}))).collect::<BTreeMap<_,_>>(),"sdks":pins.sdks.iter().map(|(name,s)|(name,&s.sha256)).collect::<BTreeMap<_,_>>(),"env":profile.env});
    if !in_place_tools.is_empty() {
        contract["in_place_tools"] = json!(
            in_place_tools
                .iter()
                .map(|name| (name, &tools[name].source_path))
                .collect::<BTreeMap<_, _>>()
        );
    }
    let contract_sha256 = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&contract)?));
    ensure!(!cancel.is_cancelled(), "toolchain preparation cancelled");
    ensure!(!final_path.exists(), "toolchain destination already exists");
    std::fs::rename(candidate, final_path)?;
    Ok(Verified {
        in_place_tools,
        format: FORMAT.into(),
        contract_sha256,
        contract,
        tools,
        sdks,
    })
}
/// Materialize verified inputs inside the stage boundary and resolve only whole values.
pub fn stage(
    verified: &Verified,
    workspace: &Path,
    command: &mut crate::model::CommandSpec,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    let base = workspace.join(".preflight/toolchain");
    std::fs::create_dir_all(&base)?;
    let mut paths = BTreeMap::new();
    for (kind, inputs, prefix) in [
        ("tools", &verified.tools, "pinned"),
        ("sdks", &verified.sdks, "sdk"),
    ] {
        std::fs::create_dir(base.join(kind))?;
        for (name, input) in inputs {
            let destination = base.join(kind).join(name);
            let actual = if kind == "tools" && verified.in_place_tools.contains(name) {
                let mut artifact = input.artifact.clone();
                artifact.path = input.source_path.clone();
                verify_original(&artifact)?;
                input.source_path.clone()
            } else {
                artifacts::verify_copy(&input.artifact, &destination)?;
                destination
            };
            paths.insert(
                format!("${{{prefix}.{name}}}"),
                actual.to_string_lossy().into_owned(),
            );
        }
    }
    fn replace(value: &str, paths: &BTreeMap<String, String>) -> Result<String> {
        if value.contains("${pinned") || value.contains("${sdk") {
            return paths
                .get(value)
                .cloned()
                .ok_or_else(|| invalid("unknown or non-whole pinned tool/SDK reference"));
        }
        Ok(value.to_owned())
    }
    command.program = replace(&command.program, &paths)?;
    command.args = command
        .args
        .iter()
        .map(|v| replace(v, &paths))
        .collect::<Result<_>>()?;
    for value in env.values_mut() {
        *value = replace(value, &paths)?;
    }
    Ok(())
}

/// Native run-in-place tools retain their original loader path; bytes are checked again just before exec.
pub(crate) fn verify_original(artifact: &Artifact) -> Result<()> {
    ensure!(
        artifact.path.canonicalize()? == artifact.path,
        "verified executable path changed"
    );
    let meta = std::fs::symlink_metadata(&artifact.path)?;
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.len() == artifact.size,
        "verified executable metadata changed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o111 != 0,
            "verified executable is not executable"
        );
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&artifact.path)?
        .take(artifact.size + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == artifact.size
            && crate::hashing::hex(Sha256::digest(bytes)) == artifact.sha256,
        "verified executable digest mismatch"
    );
    Ok(())
}
pub(crate) fn original_program(verified: &Verified, program: &str) -> Option<Artifact> {
    verified
        .in_place_tools
        .iter()
        .filter_map(|name| verified.tools.get(name))
        .find(|input| input.source_path == Path::new(program))
        .map(|input| {
            let mut a = input.artifact.clone();
            a.path = input.source_path.clone();
            a
        })
}

#[cfg(test)]
mod original_tool_tests {
    use super::*;
    #[test]
    fn original_tool_recheck_refuses_mutation_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tool");
        std::fs::write(&path, b"original").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let artifact = Artifact {
            run_id: "run".into(),
            stage_id: "stage".into(),
            name: "tool".into(),
            path: path.canonicalize().unwrap(),
            kind: ArtifactKind::Executable,
            sha256: crate::hashing::hex(Sha256::digest(b"original")),
            size: 8,
        };
        verify_original(&artifact).unwrap();
        std::fs::write(&path, b"modified").unwrap();
        assert!(verify_original(&artifact).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            let other = temp.path().join("other");
            std::fs::write(&other, b"original").unwrap();
            std::os::unix::fs::symlink(&other, &path).unwrap();
            assert!(verify_original(&artifact).is_err());
        }
    }
}
