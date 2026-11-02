//! Operator-owned worker policy; job payloads never choose the Docker executable.
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub docker_command: PathBuf,
    pub max_memory_mib: u64,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.docker_command.is_absolute()
                && self
                    .docker_command
                    .to_str()
                    .is_some_and(|s| s.len() <= 4096 && !s.chars().any(char::is_control)),
            "worker Docker executable must be an absolute path"
        );
        ensure!(
            (16..=1048576).contains(&self.max_memory_mib),
            "worker max_memory_mib must be 16..1048576"
        );
        Ok(())
    }
    pub fn config(&self, root: &Path, memory: u64) -> Result<crate::config::Config> {
        self.validate()?;
        ensure!(
            memory >= 16 && memory <= self.max_memory_mib,
            "worker container memory exceeds operator policy"
        );
        let config = serde_json::from_value(
            serde_json::json!({"version":1,"bind":"127.0.0.1:0","data_dir":root,"allowed_roots":[root],"docker_command":self.docker_command,"memory_budget_mib":self.max_memory_mib,"default_stage_memory_mib":memory}),
        )?;
        Ok(config)
    }
}
pub fn load(root: &Path) -> Result<Option<Policy>> {
    let path = root.join("runtime.json");
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.len() <= 8192,
        "invalid worker runtime policy file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "worker runtime policy must be private (0600)"
        );
    }
    let value: Policy = serde_json::from_slice(&std::fs::read(path)?)?;
    value.validate()?;
    Ok(Some(value))
}
pub fn save(root: &Path, mut value: Policy) -> Result<()> {
    value.validate()?;
    ensure!(
        root.is_absolute(),
        "worker state directory must be absolute"
    );
    for ancestor in root.ancestors() {
        if let Ok(meta) = std::fs::symlink_metadata(ancestor) {
            ensure!(
                !meta.is_symlink(),
                "worker state ancestors must not be symlinks"
            );
        }
    }
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(root)?;
    ensure!(
        root.is_absolute() && root.canonicalize()? == root,
        "worker state directory must be canonical"
    );
    value.docker_command = value
        .docker_command
        .canonicalize()
        .context("worker Docker executable does not exist")?;
    ensure!(
        value.docker_command.is_file(),
        "worker Docker executable must be a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "worker state must be private (0700)"
        );
    }
    if load(root)?.as_ref() == Some(&value) {
        return Ok(());
    }
    if let Ok(meta) = std::fs::symlink_metadata(root.join("client.lock")) {
        ensure!(!meta.is_symlink(), "worker lock must not be a symlink");
    }
    let _lock = crate::worker_client::StateLock::acquire(&root.join("client.lock"))
        .context("stop the worker before changing its runtime policy")?;
    if root.join("identity.json").exists() {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
        ensure!(
            state.get("pending").is_none_or(serde_json::Value::is_null),
            "recover pending worker job before changing runtime policy"
        );
    }
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(&value)?)?;
    file.as_file().sync_all()?;
    file.persist(root.join("runtime.json"))?;
    #[cfg(unix)]
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_is_explicit_private_and_memory_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(load(&root).unwrap().is_none());
        let policy = Policy {
            docker_command: std::env::current_exe().unwrap().canonicalize().unwrap(),
            max_memory_mib: 1024,
        };
        save(&root, policy.clone()).unwrap();
        assert_eq!(load(&root).unwrap(), Some(policy.clone()));
        assert!(policy.config(&root, 1025).is_err());
        assert!(policy.config(&root, 15).is_err());
        assert_eq!(
            policy.config(&root, 512).unwrap().default_stage_memory_mib,
            512
        );
        let lock = crate::worker_client::StateLock::acquire(&root.join("client.lock")).unwrap();
        save(&root, policy.clone()).unwrap();
        let mut changed = policy.clone();
        changed.max_memory_mib = 512;
        assert!(save(&root, changed.clone()).is_err());
        drop(lock);
        std::fs::write(
            root.join("identity.json"),
            br#"{"pending":{"job_id":"retained"}}"#,
        )
        .unwrap();
        assert!(save(&root, changed).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("runtime.json"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert!(load(&root).is_err());
        }
    }
}
