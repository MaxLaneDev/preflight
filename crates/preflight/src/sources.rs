use crate::{model::invalid, pipeline::PipelineFile};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SOURCE_FORMAT: &str = "preflight-git-source-v1";
pub const MAX_PATCH_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    /// Descriptive name, not a filesystem path.
    pub name: String,
    /// Unified diff text. Applied without a shell to an isolated snapshot.
    pub diff: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    /// Exact full object ID (SHA-1 or SHA-256). Branch names are not accepted.
    pub commit: String,
    #[serde(default)]
    pub patches: Vec<Patch>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartGitPipeline {
    pub project_id: String,
    pub definition: PipelineFile,
    pub source: GitSource,
}
impl GitSource {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.commit.len(), 40 | 64)
                && self
                    .commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            invalid("commit must be a full lowercase Git object ID")
        );
        ensure!(
            self.patches.len() <= 16,
            invalid("at most 16 ordered patches are supported")
        );
        let mut total = 0usize;
        for patch in &self.patches {
            ensure!(
                !patch.name.trim().is_empty()
                    && patch.name.len() <= 128
                    && !patch.name.chars().any(char::is_control),
                invalid("patch name must be 1..128 bytes without control characters")
            );
            ensure!(
                !patch.diff.trim().is_empty() && !patch.diff.contains('\0'),
                invalid("patch must be nonempty text without NUL")
            );
            total = total
                .checked_add(patch.diff.len())
                .ok_or_else(|| invalid("patch size overflow"))?;
            ensure!(
                total <= MAX_PATCH_BYTES,
                invalid("ordered patch text exceeds 32 KiB")
            );
        }
        Ok(())
    }
    /// Compact identity only: raw patch contents belong in durable source evidence.
    pub fn identity(&self) -> Result<serde_json::Value> {
        self.validate()?;
        let patches=self.patches.iter().enumerate().map(|(ordinal,patch)|serde_json::json!({"ordinal":ordinal,"name":patch.name,"sha256":crate::hashing::hex(Sha256::digest(patch.diff.as_bytes())),"bytes":patch.diff.len()})).collect::<Vec<_>>();
        Ok(serde_json::json!({"format":SOURCE_FORMAT,"commit":self.commit,"patches":patches}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_commit_and_ordered_patch_identity() {
        let mut source = GitSource {
            commit: "a".repeat(40),
            patches: vec![
                Patch {
                    name: "first".into(),
                    diff: "first diff\n".into(),
                },
                Patch {
                    name: "second".into(),
                    diff: "second diff\n".into(),
                },
            ],
        };
        let before = source.identity().unwrap();
        source.patches.reverse();
        let after = source.identity().unwrap();
        assert_ne!(before, after);
        assert_eq!(
            before["patches"][0]["sha256"],
            after["patches"][1]["sha256"]
        );
        for value in ["HEAD", "main", "--upload-pack=evil", "abc", "../outside"] {
            source.commit = value.into();
            assert!(source.validate().is_err());
        }
        source.commit = "b".repeat(64);
        assert!(source.validate().is_ok());
    }
    #[test]
    fn bounds_are_on_total_patch_text() {
        let mut source = GitSource {
            commit: "0".repeat(40),
            patches: vec![
                Patch {
                    name: "a".into(),
                    diff: "x".repeat(MAX_PATCH_BYTES / 2 + 1),
                },
                Patch {
                    name: "b".into(),
                    diff: "x".repeat(MAX_PATCH_BYTES / 2),
                },
            ],
        };
        assert!(source.validate().is_err());
        source.patches.pop();
        assert!(source.validate().is_ok());
        source.patches[0].diff = "\0".into();
        assert!(source.validate().is_err());
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchQuery {
    pub run_id: String,
    pub ordinal: usize,
    /// For a named root in a multi-repository run; omit for a single Git source.
    pub root_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct DirtySource {
    /// Optional full HEAD ID required at preparation; movable refs are not accepted.
    pub expected_head: Option<String>,
    /// Exact project-relative untracked file paths. No globs or implicit recursion.
    #[serde(default)]
    pub untracked: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartDirtyPipeline {
    pub project_id: String,
    pub definition: PipelineFile,
    #[serde(default)]
    pub source: DirtySource,
}
impl DirtySource {
    pub fn validate(&self) -> Result<()> {
        if let Some(head) = &self.expected_head {
            GitSource {
                commit: head.clone(),
                patches: vec![],
            }
            .validate()?;
        }
        ensure!(
            self.untracked.len() <= 256,
            invalid("at most 256 explicit untracked files are supported")
        );
        let mut seen = std::collections::BTreeSet::new();
        for path in &self.untracked {
            crate::pipeline::relative(path)?;
            ensure!(
                !path.is_empty()
                    && path.len() <= 4096
                    && path.split('/').count() <= 64
                    && path.split('/').all(|part| !part.is_empty()
                        && !matches!(
                            part,
                            "." | ".." | ".git" | ".preflight" | "target" | "node_modules"
                        )),
                invalid("untracked path must be canonical and outside excluded directories")
            );
            ensure!(seen.insert(path), invalid("duplicate untracked path"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod dirty_contract_tests {
    use super::*;
    #[test]
    fn explicit_untracked_paths_and_expected_head_are_validated() {
        let mut request = DirtySource::default();
        request.validate().unwrap();
        request.untracked = vec!["tests/new-case.sx".into()];
        request.validate().unwrap();
        for path in [
            "../escape",
            "/absolute",
            "./alias",
            "a//b",
            ".git/config",
            "target/output",
            "",
        ] {
            request.untracked = vec![path.into()];
            assert!(request.validate().is_err(), "{path:?}");
        }
        request.untracked = vec!["same".into(), "same".into()];
        assert!(request.validate().is_err());
        request.untracked.clear();
        request.expected_head = Some("HEAD".into());
        assert!(request.validate().is_err());
        request.expected_head = Some("a".repeat(40));
        request.validate().unwrap();
    }
}

/// Untagged storage preserves existing exact-commit request JSON from schema 9.
/// Both variants reject unknown fields, so ambiguous source modes are refused.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceRequest {
    Git(GitSource),
    Dirty(DirtySource),
    Multi(crate::multi_source::MultiSource),
}
impl SourceRequest {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Git(_) => "git_commit",
            Self::Dirty(_) => "git_dirty",
            Self::Multi(_) => "multi_repository",
        }
    }
    pub fn identity(&self) -> Result<serde_json::Value> {
        match self {
            Self::Git(s) => s.identity(),
            Self::Multi(s) => s.identity(),
            Self::Dirty(s) => {
                s.validate()?;
                Ok(
                    serde_json::json!({"format":"preflight-dirty-source-v1","expected_head":s.expected_head,"untracked":s.untracked}),
                )
            }
        }
    }
}

#[cfg(test)]
mod stored_source_tests {
    use super::*;
    #[test]
    fn schema_nine_git_json_remains_readable_and_modes_cannot_mix() {
        let old = serde_json::json!({"commit":"a".repeat(40),"patches":[]});
        let parsed: SourceRequest = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(parsed.mode(), "git_commit");
        assert_eq!(serde_json::to_value(parsed).unwrap(), old);
        assert!(
            serde_json::from_value::<SourceRequest>(
                serde_json::json!({"commit":"a".repeat(40),"untracked":[]})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::from_value::<SourceRequest>(serde_json::json!({"untracked":["file"]}))
                .unwrap()
                .mode(),
            "git_dirty"
        );
    }
}
