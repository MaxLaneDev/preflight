use crate::{
    db::Db,
    model::Artifact,
    pipeline::{self, ArtifactKind, StageDefinition},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAX_BYTES: u64 = 128 * 1024 * 1024;

pub fn publish(
    run_id: &str,
    run_root: &Path,
    stage: &StageDefinition,
    cwd: &Path,
) -> Result<Vec<Artifact>> {
    if stage.outputs.is_empty() {
        return Ok(vec![]);
    }
    let root = pipeline::contained(run_root, "artifacts")?;
    std::fs::create_dir_all(&root)?;
    let temporary = root.join(format!(".pending-{}", stage.id));
    let published = root.join(&stage.id);
    std::fs::create_dir(&temporary)?;
    let mut artifacts = vec![];
    let mut total = 0;
    for (name, spec) in &stage.outputs {
        let source = pipeline::contained(cwd, &spec.path)?;
        let (sha256, size, _) = copy_artifact(&source, &temporary.join(name), spec.kind)?;
        total += size;
        ensure!(total <= MAX_BYTES, "stage outputs exceed 128 MiB");
        artifacts.push(Artifact {
            run_id: run_id.into(),
            stage_id: stage.id.clone(),
            name: name.clone(),
            kind: spec.kind,
            sha256,
            size,
            path: published.join(name),
        });
    }
    // Publish the complete output set before the SQLite success transaction makes it visible.
    std::fs::rename(temporary, published)?;
    Ok(artifacts)
}
pub fn prepare_inputs(
    db: &Db,
    run_id: &str,
    stage: &StageDefinition,
    workspace: &Path,
) -> Result<BTreeMap<(String, String), PathBuf>> {
    let mut refs = BTreeSet::new();
    for value in std::iter::once(&stage.command.program)
        .chain(&stage.command.args)
        .chain(stage.inputs.values())
        .chain(stage.container.iter().filter_map(|c| c.image_from.as_ref()))
    {
        if let Some((dep, name)) = pipeline::artifact_ref(value)? {
            refs.insert((dep.to_owned(), name.to_owned()));
        }
    }
    let artifacts = db.artifacts(run_id)?;
    let mut paths = BTreeMap::new();
    ensure!(
        !workspace.join(".preflight").exists(),
        "source uses reserved .preflight directory"
    );
    for value in std::iter::once(&stage.command.program)
        .chain(&stage.command.args)
        .chain(stage.inputs.values())
        .chain(stage.container.iter().filter_map(|c| c.image_from.as_ref()))
    {
        if let Some(digest) = crate::modules::reference(value)? {
            let key = ("$module".to_string(), digest.to_string());
            if paths.contains_key(&key) {
                continue;
            }
            let artifact = db.compatible_module_for_stage(run_id, digest, stage)?;
            ensure!(
                value != &stage.command.program || artifact.kind == ArtifactKind::Executable,
                "module program must be executable"
            );
            let parent = workspace.join(".preflight/modules");
            std::fs::create_dir_all(&parent)?;
            let destination = parent.join(digest);
            verify_copy(&artifact, &destination)?;
            db.record_module_input(run_id, &stage.id, &destination, &artifact)?;
            paths.insert(key, destination);
        }
    }
    for (dep, name) in refs {
        let artifact = artifacts
            .iter()
            .find(|a| a.stage_id == dep && a.name == name)
            .context("upstream artifact is not published")?;
        let parent = workspace.join(".preflight/inputs").join(&dep);
        std::fs::create_dir_all(&parent)?;
        let path = parent.join(&name);
        verify_copy(artifact, &path)?;
        paths.insert((dep, name), path);
    }
    for (destination, reference) in &stage.inputs {
        if let Some(digest) = crate::modules::reference(reference)? {
            let artifact = db.compatible_module_for_stage(run_id, digest, stage)?;
            let destination = pipeline::contained(workspace, destination)?;
            ensure!(
                !destination.exists(),
                "module input would overwrite a source file"
            );
            std::fs::create_dir_all(destination.parent().context("module input parent")?)?;
            verify_copy(&artifact, &destination)?;
            db.record_module_input(run_id, &stage.id, &destination, &artifact)?;
            continue;
        }
        let (dep, name) = pipeline::artifact_ref(reference)?.context("input reference")?;
        let artifact = artifacts
            .iter()
            .find(|a| a.stage_id == dep && a.name == name)
            .context("input artifact")?;
        let destination = pipeline::contained(workspace, destination)?;
        ensure!(
            !destination.exists(),
            "artifact input would overwrite a source file"
        );
        std::fs::create_dir_all(destination.parent().context("input parent")?)?;
        verify_copy(artifact, &destination)?;
    }
    Ok(paths)
}
pub(crate) fn verify_copy(
    artifact: &Artifact,
    destination: &Path,
) -> Result<Option<crate::trees::Manifest>> {
    ensure!(
        !std::fs::symlink_metadata(&artifact.path)?
            .file_type()
            .is_symlink(),
        "published artifact became a symlink"
    );
    let (digest, size, manifest) = copy_artifact(&artifact.path, destination, artifact.kind)?;
    ensure!(
        digest == artifact.sha256 && size == artifact.size,
        "published artifact digest mismatch"
    );
    Ok(manifest)
}
pub fn substitute(value: &str, paths: &BTreeMap<(String, String), PathBuf>) -> Result<String> {
    if let Some(digest) = crate::modules::reference(value)? {
        return Ok(paths
            .get(&("$module".into(), digest.into()))
            .context("module was not materialized")?
            .to_str()
            .context("module path is not UTF-8")?
            .into());
    }
    match pipeline::artifact_ref(value)? {
        None => Ok(value.into()),
        Some((dep, name)) => Ok(paths
            .get(&(dep.into(), name.into()))
            .context("artifact reference was not materialized")?
            .to_str()
            .context("artifact path is not UTF-8")?
            .into()),
    }
}
pub(crate) fn copy_hashed(
    source: &Path,
    destination: &Path,
    kind: ArtifactKind,
) -> Result<(String, u64)> {
    copy_hashed_bounded(source, destination, kind, MAX_BYTES)
}
pub(crate) fn copy_hashed_bounded(
    source: &Path,
    destination: &Path,
    kind: ArtifactKind,
    remaining: u64,
) -> Result<(String, u64)> {
    let limit = remaining.min(MAX_BYTES);
    let mut input =
        File::open(source).with_context(|| format!("open declared output {}", source.display()))?;
    let metadata = input.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "artifact must be a regular file within the remaining byte budget"
    );
    #[cfg(unix)]
    if kind == ArtifactKind::Executable {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "executable output has no execute permission"
        );
    }
    #[cfg(not(unix))]
    let _ = kind;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(size <= limit, "artifact grew beyond remaining byte budget");
        digest.update(&bytes[..count]);
        output.write_all(&bytes[..count])?;
    }
    output.sync_all()?;
    let mut permissions = metadata.permissions();
    permissions.set_readonly(true);
    output.set_permissions(permissions)?;
    Ok((crate::hashing::hex(digest.finalize()), size))
}

pub(crate) fn copy_artifact(
    source: &Path,
    destination: &Path,
    kind: ArtifactKind,
) -> Result<(String, u64, Option<crate::trees::Manifest>)> {
    if kind == ArtifactKind::Directory {
        let manifest = crate::trees::copy(source, destination)?;
        Ok((manifest.sha256()?, manifest.size(), Some(manifest)))
    } else {
        let (hash, size) = copy_hashed(source, destination, kind)?;
        Ok((hash, size, None))
    }
}
