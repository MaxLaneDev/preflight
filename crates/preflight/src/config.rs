use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_command: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_tokens: Vec<crate::auth::Token>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ci_sources: Vec<crate::ci::Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_policies: Vec<crate::storage_policy::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_command: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_launcher: Option<PathBuf>,
    /// Optional separate worker-only loopback gateway; remote access requires TLS proxy or SSH tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_bind: Option<SocketAddr>,
    /// Optional admission budget; native processes are not memory-sandboxed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_budget_mib: Option<u64>,
    /// Optional free-space admission floor. Running commands are never interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_free_disk_bytes: Option<u64>,
    /// Admission threshold for logical file bytes in data_dir, not a filesystem quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_storage_mib: Option<u64>,
    /// Optional private administrative credential file; contents are never serialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_token_file: Option<PathBuf>,
    #[serde(default = "default_stage_memory")]
    pub default_stage_memory_mib: u64,
    /// Operator-controlled executable; absent means no Codex message delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_command: Option<PathBuf>,
    pub version: u32,
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub allowed_roots: Vec<PathBuf>,
    #[serde(default = "default_parallel")]
    pub max_parallel_commands: usize,
    #[serde(default = "default_active_runs")]
    pub max_active_runs: usize,
}
fn default_active_runs() -> usize {
    32
}
fn default_stage_memory() -> u64 {
    256
}
fn default_parallel() -> usize {
    2
}
impl Config {
    pub async fn check_disk_admission(&self) -> Result<()> {
        if self.max_storage_mib.is_none() && self.min_free_disk_bytes.is_none() {
            return Ok(());
        }
        let config = self.clone();
        tokio::task::spawn_blocking(move || config.check_disk_admission_blocking()).await?
    }
    fn check_disk_admission_blocking(&self) -> Result<()> {
        if let Some(maximum) = self.max_storage_mib {
            check_storage_size(&self.data_dir, maximum * 1024 * 1024, 200_000)?;
        }
        if let Some(minimum) = self.min_free_disk_bytes {
            crate::storage_gc::directory(&self.data_dir)?;
            let available = fs2::available_space(&self.data_dir)?;
            ensure!(
                available >= minimum,
                "disk_admission_denied: available {available} bytes, required {minimum}; running commands are unchanged"
            );
        }
        Ok(())
    }

    pub fn validate_memory(&self) -> Result<()> {
        if let Some(repository) = &self.release_repository {
            crate::releases::validate(repository)?;
        }
        if let Some(maximum) = self.max_storage_mib {
            ensure!(
                (1..=8_589_934_591).contains(&maximum),
                "max_storage_mib must be 1..8589934591 or omitted"
            );
        }
        crate::auth::validate(&self.access_tokens)?;
        ensure!(
            self.ci_sources.len() <= 32,
            "at most 32 optional CI sources are supported"
        );
        let mut names = std::collections::BTreeSet::new();
        for source in &self.ci_sources {
            source.validate()?;
            ensure!(names.insert(&source.name), "CI source names must be unique");
        }

        if let Some(minimum) = self.min_free_disk_bytes {
            ensure!(
                minimum > 0 && minimum <= 9_007_199_254_740_991,
                "min_free_disk_bytes must be 1..9007199254740991 or omitted"
            );
        }

        ensure!(
            self.storage_policies.len() <= 32,
            "at most 32 storage policies supported"
        );
        let mut names = std::collections::BTreeSet::new();
        for policy in &self.storage_policies {
            policy.validate()?;
            ensure!(names.insert(&policy.name), "duplicate storage policy name");
        }
        ensure!(
            (1..=32).contains(&self.max_active_runs),
            "max_active_runs must be 1..32"
        );
        if let Some(bind) = self.worker_bind {
            ensure!(
                bind.ip().is_loopback() && bind != self.bind,
                "worker_bind must be a separate loopback address"
            );
        }
        ensure!(
            self.default_stage_memory_mib > 0,
            "default_stage_memory_mib must be positive"
        );
        if let Some(budget) = self.memory_budget_mib {
            ensure!(
                budget > 0 && self.default_stage_memory_mib <= budget,
                "memory_budget_mib must cover default_stage_memory_mib"
            );
        }
        Ok(())
    }
    pub fn stage_memory(&self, stage: &crate::pipeline::StageDefinition) -> Result<u64> {
        let declared = stage.resources.as_ref().and_then(|r| r.memory_mib);
        match (self.memory_budget_mib, declared) {
            (None, None) => Ok(0),
            (None, Some(_)) => anyhow::bail!(crate::model::invalid(
                "stage memory_mib requires configured server memory_budget_mib"
            )),
            (Some(budget), request) => {
                let request = request.unwrap_or(self.default_stage_memory_mib);
                ensure!(
                    request > 0 && request <= budget,
                    crate::model::invalid(format!(
                        "stage {} memory request must be 1..{} MiB",
                        stage.id, budget
                    ))
                );
                Ok(request)
            }
        }
    }
    pub fn preparation_memory(&self) -> u64 {
        self.memory_budget_mib
            .map_or(0, |_| self.default_stage_memory_mib)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self =
            toml::from_str(&std::fs::read_to_string(path).context("read config")?)?;
        ensure!(config.version == 1, "unsupported config version");
        ensure!(
            config.bind.ip().is_loopback(),
            "only loopback binding is supported"
        );
        ensure!(
            (1..=32).contains(&config.max_parallel_commands),
            "max_parallel_commands must be 1..32"
        );
        config.validate_memory()?;
        let base = path
            .canonicalize()?
            .parent()
            .context("config parent")?
            .to_path_buf();
        config.data_dir = base.join(&config.data_dir);
        if let Some(file) = &mut config.admin_token_file {
            *file = base.join(&*file);
            crate::settings::file_credential(file)?;
        }
        for root in &mut config.allowed_roots {
            *root = base.join(&*root).canonicalize()?;
        }
        Ok(config)
    }
}
// Fail closed on incomplete accounting; never follow links outside managed storage.
fn check_storage_size(root: &Path, maximum: u64, mut remaining: usize) -> Result<()> {
    crate::storage_gc::directory(root)?;
    let mut pending = vec![std::fs::read_dir(root)?];
    let mut bytes = 0u64;
    while let Some(entries) = pending.last_mut() {
        let Some(entry) = entries.next() else {
            pending.pop();
            continue;
        };
        ensure!(
            remaining > 0,
            "disk_admission_denied: storage scan exceeds 200000 entries; reduce stored data or disable the size threshold"
        );
        remaining -= 1;
        let path = entry?.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            ensure!(
                pending.len() < 256,
                "disk_admission_denied: storage directory nesting exceeds 256 levels"
            );
            pending.push(std::fs::read_dir(path)?);
        } else if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
            ensure!(
                bytes < maximum,
                "disk_admission_denied: managed storage uses at least {bytes} bytes, threshold {maximum}; running commands are unchanged"
            );
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn storage_admission_counts_nested_files_and_rejects_at_threshold() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        std::fs::create_dir_all(root.join("workspaces/run")).unwrap();
        let mut config: Config = toml::from_str("version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=[]\nmax_storage_mib=1").unwrap();
        config.data_dir = root.clone();
        config.validate_memory().unwrap();
        config.check_disk_admission().await.unwrap();
        let file = std::fs::File::create(root.join("workspaces/run/output")).unwrap();
        file.set_len(1024 * 1024).unwrap();
        assert!(
            config
                .check_disk_admission()
                .await
                .unwrap_err()
                .to_string()
                .contains("disk_admission_denied")
        );
        assert!(check_storage_size(&root, 2 * 1024 * 1024, 1).is_err());
        config.max_storage_mib = None;
        config.check_disk_admission().await.unwrap();
        for value in [0, 8_589_934_592] {
            config.max_storage_mib = Some(value);
            assert!(config.validate_memory().is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn storage_admission_does_not_follow_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(temp.path().join("outside"), [0; 100]).unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("link")).unwrap();
        check_storage_size(&root, 1, 10).unwrap();
    }
    #[test]
    fn config_validates_version_bind_and_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let valid = "version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=['.']\n";
        std::fs::write(&path, valid).unwrap();
        assert!(Config::load(&path).is_ok());
        std::fs::write(&path, valid.replace("['.']", "[]")).unwrap();
        assert!(Config::load(&path).unwrap().allowed_roots.is_empty());
        std::fs::write(
            &path,
            format!("{valid}memory_budget_mib=100\ndefault_stage_memory_mib=20"),
        )
        .unwrap();
        let configured = Config::load(&path).unwrap();
        assert_eq!(configured.memory_budget_mib, Some(100));
        for bad in [
            valid.replace("version=1", "version=2"),
            valid.replace("127.0.0.1", "0.0.0.0"),
            format!("{valid}typo=1"),
            format!("{valid}max_active_runs=0"),
            format!("{valid}max_active_runs=33"),
            format!("{valid}memory_budget_mib=0"),
            format!("{valid}memory_budget_mib=128"),
            format!("{valid}default_stage_memory_mib=0"),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(Config::load(&path).is_err());
        }
    }
}
