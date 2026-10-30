use crate::{
    inputs,
    model::{Run, invalid},
    pipeline::PipelineFile,
    sources::{DirtySource, GitSource},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub const FORMAT: &str = "preflight-multi-source-v1";
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "mode",
    content = "source",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RootMode {
    Git(GitSource),
    Dirty(DirtySource),
}
impl RootMode {
    fn identity(&self) -> Result<Value> {
        match self {
            Self::Git(s) => s.identity(),
            Self::Dirty(s) => crate::sources::SourceRequest::Dirty(s.clone()).identity(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceRoot {
    pub name: String,
    pub project_id: String,
    /// Non-overlapping relative directory inside the combined source workspace.
    pub path: String,
    pub revision: RootMode,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MultiSource {
    pub roots: Vec<SourceRoot>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartMultiPipeline {
    pub project_id: String,
    pub definition: PipelineFile,
    pub source: MultiSource,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RootInputsQuery {
    pub run_id: String,
    pub root_name: String,
    #[serde(default)]
    pub offset: usize,
}
impl MultiSource {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=8).contains(&self.roots.len()),
            invalid("source set requires 1..8 roots")
        );
        let mut names = BTreeSet::new();
        let mut paths: Vec<PathBuf> = vec![];
        let mut mount_components = std::collections::BTreeMap::new();
        let mut patch_bytes = 0usize;
        for root in &self.roots {
            ensure!(
                !root.name.is_empty()
                    && root.name.len() <= 64
                    && root
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
                invalid("root name must be 1..64 ASCII letters, digits, underscores or hyphens")
            );
            ensure!(
                names.insert(root.name.to_ascii_lowercase()),
                invalid("duplicate root name")
            );
            ensure!(
                !root.project_id.is_empty() && root.project_id.len() <= 128,
                invalid("invalid root project ID")
            );
            // Portable mount paths make case aliases and excluded directories unambiguous.
            ensure!(
                !root.path.is_empty()
                    && root.path.len() <= 256
                    && root.path.split('/').count() <= 8
                    && root.path.split('/').all(|part| !part.is_empty()
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                        && !matches!(
                            part.to_ascii_lowercase().as_str(),
                            "target" | "node_modules" | "con" | "prn" | "aux" | "nul"
                        )
                        && !(part.len() == 4
                            && (part.to_ascii_lowercase().starts_with("com")
                                || part.to_ascii_lowercase().starts_with("lpt"))
                            && part.as_bytes()[3].is_ascii_digit())),
                invalid("root path must contain portable relative directory names")
            );
            let path = PathBuf::from(root.path.to_ascii_lowercase());
            ensure!(
                paths
                    .iter()
                    .all(|p| !p.starts_with(&path) && !path.starts_with(p)),
                invalid("source root paths overlap")
            );
            for prefix in Path::new(&root.path)
                .ancestors()
                .filter(|p| !p.as_os_str().is_empty())
            {
                let text = prefix.to_str().context("mount path UTF-8")?;
                if let Some(previous) =
                    mount_components.insert(text.to_ascii_lowercase(), text.to_owned())
                {
                    ensure!(
                        previous == text,
                        invalid("source mount paths contain case aliases")
                    );
                }
            }
            paths.push(path);
            root.revision.identity()?;
            if let RootMode::Git(s) = &root.revision {
                patch_bytes += s.patches.iter().map(|p| p.diff.len()).sum::<usize>();
            }
        }
        ensure!(
            patch_bytes <= crate::sources::MAX_PATCH_BYTES,
            invalid("source set patches exceed 32 KiB total")
        );
        Ok(())
    }
    pub fn identity(&self) -> Result<Value> {
        self.validate()?;
        let mut roots: Vec<_> = self.roots.iter().collect();
        roots.sort_by_key(|r| &r.name);
        Ok(
            json!({"format":FORMAT,"roots":roots.into_iter().map(|r| Ok(json!({"name":r.name,"project_id":r.project_id,"path":r.path,"revision":r.revision.identity()?}))).collect::<Result<Vec<_>>>()?}),
        )
    }
}

fn include_manifest(
    mount: &str,
    entries: &std::collections::BTreeMap<String, inputs::Entry>,
    total_bytes: &mut u64,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for (path, entry) in entries {
        *total_bytes = total_bytes
            .checked_add(entry.size)
            .context("source set size overflow")?;
        paths.insert(Path::new(mount).join(path));
    }
    for prefix in Path::new(mount)
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
    {
        paths.insert(prefix.to_path_buf());
    }
    ensure!(
        *total_bytes <= 128 * 1024 * 1024 && paths.len() <= 20_000,
        invalid("combined sources exceed 128 MiB or 20000 entries")
    );
    Ok(())
}

pub async fn prepare(
    resolved: Vec<(SourceRoot, PathBuf)>,
    destination: &Path,
    data: &Path,
    run: &Run,
    cancel: &CancellationToken,
) -> Result<Value> {
    MultiSource {
        roots: resolved.iter().map(|(r, _)| r.clone()).collect(),
    }
    .validate()?;
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let work = async {
        let parent = destination.parent().context("source set parent")?;
        std::fs::create_dir_all(parent)?;
        let pending = tempfile::tempdir_in(parent)?;
        let candidate = pending.path().join("tree");
        std::fs::create_dir(&candidate)?;
        let mut roots = vec![];
        let mut total_bytes = 0u64;
        let mut combined_paths = BTreeSet::new();
        let mut identities = vec![];
        let mut resolved = resolved;
        resolved.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
        for (root, repository) in resolved {
            let target = candidate.join(&root.path);
            let mut provenance = match &root.revision {
                RootMode::Git(s) => {
                    crate::git_source::prepare(&repository, &target, data, s, &token).await?
                }
                RootMode::Dirty(s) => {
                    crate::dirty_source::prepare(&repository, &target, data, s, &token).await?
                }
            };
            provenance["snapshot_path"] = json!(destination.join(&root.path));
            let capture_root = target.clone();
            let capture_run = run.clone();
            let manifest =
                tokio::task::spawn_blocking(move || inputs::capture(&capture_root, &capture_run))
                    .await??;
            include_manifest(
                &root.path,
                &manifest.entries,
                &mut total_bytes,
                &mut combined_paths,
            )?;
            identities.push(json!({"name":root.name,"path":root.path,"tree_sha256":manifest.tree_sha256,"source":root.revision.identity()?,"resolved_commit":provenance["resolved_commit"],"index_inventory_sha256":provenance.get("index_inventory_sha256")}));
            roots.push(json!({"name":root.name,"project_id":root.project_id,"path":root.path,"prepared":provenance,"manifest":manifest}));
        }
        let mut digest = Sha256::new();
        digest.update(FORMAT.as_bytes());
        digest.update([0]);
        digest.update(serde_json::to_vec(&identities)?);
        ensure!(!token.is_cancelled(), "source set cancelled");
        ensure!(
            !destination.exists(),
            "source set destination already exists"
        );
        std::fs::rename(&candidate, destination)?;
        Ok(
            json!({"format":FORMAT,"aggregate_sha256":crate::hashing::hex(digest.finalize()),"roots":roots,"consistency":"individually checked sources; not an atomic cross-repository snapshot"}),
        )
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("source set cancelled"),
        result = tokio::time::timeout(std::time::Duration::from_secs(120), work) => result.context("source set preparation timed out")?,
    }
}

/// Keep source evidence compact; manifests are read through the paged root endpoint.
pub fn summary(mut value: Value) -> Value {
    if value["format"] == FORMAT
        && let Some(roots) = value["roots"].as_array_mut()
    {
        for root in roots {
            if let Some(manifest) = root["manifest"].as_object_mut()
                && let Some(entries) = manifest.remove("entries")
            {
                manifest.insert(
                    "total".into(),
                    json!(entries.as_object().map_or(0, |e| e.len())),
                );
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root(name: &str, path: &str) -> SourceRoot {
        SourceRoot {
            name: name.into(),
            project_id: "project".into(),
            path: path.into(),
            revision: RootMode::Dirty(DirtySource::default()),
        }
    }
    #[test]
    fn refuses_collisions_aliases_traversal_and_unbounded_sets() {
        for paths in [
            ("app", "app"),
            ("app", "app/dep"),
            ("App", "app"),
            ("Roots/app", "roots/dep"),
        ] {
            assert!(
                MultiSource {
                    roots: vec![root("a", paths.0), root("b", paths.1)]
                }
                .validate()
                .is_err()
            );
        }
        for path in [
            "",
            ".",
            "../other",
            "/absolute",
            ".git",
            ".preflight",
            "a//b",
            "target",
            "node_modules",
            "a/CON",
            "a/LPT1",
            "a/b.",
            "a/b c",
        ] {
            assert!(
                MultiSource {
                    roots: vec![root("app", path)]
                }
                .validate()
                .is_err(),
                "{path}"
            );
        }
        assert!(MultiSource { roots: vec![] }.validate().is_err());
        assert!(
            MultiSource {
                roots: vec![root("app", "app"); 9]
            }
            .validate()
            .is_err()
        );
        assert!(
            MultiSource {
                roots: vec![root("app", "a"), root("APP", "b")]
            }
            .validate()
            .is_err()
        );
        MultiSource {
            roots: vec![root("app", "repos/app"), root("dep", "repos/dep")],
        }
        .validate()
        .unwrap();
    }
    #[test]
    fn rejects_mixed_mode_storage_and_patch_budget_is_global() {
        let mixed = json!({"roots":[],"commit":"a".repeat(40)});
        assert!(serde_json::from_value::<crate::sources::SourceRequest>(mixed).is_err());
        let mut first = root("app", "app");
        first.revision = RootMode::Git(GitSource {
            commit: "a".repeat(40),
            patches: vec![crate::sources::Patch {
                name: "p".into(),
                diff: "x".repeat(20000),
            }],
        });
        let mut second = first.clone();
        second.name = "dep".into();
        second.path = "dep".into();
        assert!(
            MultiSource {
                roots: vec![first, second]
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn quotas_cover_all_roots_and_mount_directories() {
        let entry = inputs::Entry {
            kind: "file".into(),
            sha256: Some("a".repeat(64)),
            size: 70 * 1024 * 1024,
            executable: false,
        };
        let entries = std::collections::BTreeMap::from([("file".into(), entry)]);
        let mut bytes = 0;
        let mut paths = BTreeSet::new();
        include_manifest("app", &entries, &mut bytes, &mut paths).unwrap();
        assert!(include_manifest("dep", &entries, &mut bytes, &mut paths).is_err());
        let mut paths = (0..20_000)
            .map(|n| PathBuf::from(format!("file-{n}")))
            .collect();
        assert!(
            include_manifest(
                "mount",
                &std::collections::BTreeMap::new(),
                &mut 0,
                &mut paths
            )
            .is_err()
        );
    }
}
