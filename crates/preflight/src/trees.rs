use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub const FORMAT: &str = "preflight-directory-v1";
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub kind: String,
    pub sha256: Option<String>,
    pub size: u64,
    pub executable: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub entries: BTreeMap<String, Entry>,
}
impl Manifest {
    pub fn sha256(&self) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(FORMAT.as_bytes());
        hash.update([0]);
        hash.update(serde_json::to_vec(self)?);
        Ok(crate::hashing::hex(hash.finalize()))
    }
    pub fn size(&self) -> u64 {
        self.entries.values().map(|e| e.size).sum()
    }
}

pub(crate) fn portable_component(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !name.is_empty()
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        && !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !((base.starts_with("COM") || base.starts_with("LPT"))
            && base[3..].chars().all(char::is_numeric)
            && base.len() > 3)
}

/// Hash the private copy, not a subsequent read of the mutable source.
pub fn copy(source: &Path, destination: &Path) -> Result<Manifest> {
    fn visit(
        source: &Path,
        destination: &Path,
        prefix: &str,
        depth: usize,
        manifest: &mut Manifest,
        bytes: &mut u64,
    ) -> Result<()> {
        ensure!(depth <= 64, "directory artifact exceeds depth limit");
        let metadata = std::fs::symlink_metadata(source)?;
        ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            "expected real artifact directory"
        );
        std::fs::create_dir(destination)?;
        let mut names = std::collections::BTreeSet::new();
        for item in std::fs::read_dir(source)? {
            let item = item?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("directory artifact name is not UTF-8"))?;
            ensure!(
                portable_component(&name),
                "unsupported directory artifact name"
            );
            ensure!(
                names.insert(name.to_lowercase()),
                "case-ambiguous directory artifact names"
            );
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            ensure!(
                relative.len() <= 4096,
                "directory artifact path exceeds limit"
            );
            ensure!(
                manifest.entries.len() < MAX_ENTRIES,
                "directory artifact exceeds entry limit"
            );
            let path = item.path();
            let target = destination.join(&name);
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.is_symlink(),
                "directory artifacts refuse symlinks"
            );
            let mut entry = Entry {
                kind: "directory".into(),
                sha256: None,
                size: 0,
                executable: false,
            };
            if metadata.is_dir() {
                manifest.entries.insert(relative.clone(), entry);
                visit(&path, &target, &relative, depth + 1, manifest, bytes)?;
            } else {
                ensure!(
                    metadata.is_file(),
                    "directory artifacts refuse special files"
                );
                ensure!(
                    metadata.len() <= MAX_BYTES.saturating_sub(*bytes),
                    "directory artifact exceeds 128 MiB"
                );
                let (hash, size) = crate::artifacts::copy_hashed_bounded(
                    &path,
                    &target,
                    crate::pipeline::ArtifactKind::File,
                    MAX_BYTES.saturating_sub(*bytes),
                )?;
                *bytes = bytes
                    .checked_add(size)
                    .context("directory artifact size overflow")?;
                ensure!(*bytes <= MAX_BYTES, "directory artifact exceeds 128 MiB");
                entry.kind = "file".into();
                entry.sha256 = Some(hash);
                entry.size = size;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    entry.executable =
                        std::fs::metadata(&target)?.permissions().mode() & 0o111 != 0;
                }
                manifest.entries.insert(relative, entry);
            }
        }
        Ok(())
    }
    let mut manifest = Manifest {
        format: FORMAT.into(),
        entries: BTreeMap::new(),
    };
    visit(source, destination, "", 0, &mut manifest, &mut 0)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copied_tree_identity_includes_paths_and_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("empty")).unwrap();
        std::fs::write(source.join("code"), "module").unwrap();
        let first = copy(&source, &temp.path().join("first")).unwrap();
        let repeat = copy(&source, &temp.path().join("repeat")).unwrap();
        assert_eq!(first.sha256().unwrap(), repeat.sha256().unwrap());
        assert_eq!(first.size(), 6);
        assert_eq!(first.entries["empty"].kind, "directory");
        std::fs::rename(source.join("code"), source.join("renamed")).unwrap();
        let renamed = copy(&source, &temp.path().join("renamed")).unwrap();
        assert_ne!(first.sha256().unwrap(), renamed.sha256().unwrap());
        std::fs::remove_dir(source.join("empty")).unwrap();
        let removed = copy(&source, &temp.path().join("removed")).unwrap();
        assert_ne!(renamed.sha256().unwrap(), removed.sha256().unwrap());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("first/code")).unwrap(),
            "module"
        );
    }
    #[test]
    fn rejects_nonportable_components() {
        for name in [
            "CON", "nul.txt", "LPT1", "COM2.exe", "tail.", "tail ", "a/b", "a\\b", "a:b", "a?b",
            "a\n",
        ] {
            assert!(!portable_component(name), "{name:?}");
        }
        assert!(portable_component("модуль-1.sx"));
        assert!(portable_component("lib.module"));
    }
    #[test]
    fn refuses_oversized_and_deep_trees_without_overwriting_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let large = std::fs::File::create(source.join("large")).unwrap();
        large.set_len(MAX_BYTES + 1).unwrap();
        assert!(copy(&source, &temp.path().join("too-large")).is_err());
        drop(large);
        std::fs::remove_file(source.join("large")).unwrap();
        let mut nested = source.clone();
        for _ in 0..65 {
            nested = nested.join("d");
            std::fs::create_dir(&nested).unwrap();
        }
        assert!(copy(&source, &temp.path().join("too-deep")).is_err());
        let destination = temp.path().join("existing");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("keep"), "unchanged").unwrap();
        assert!(copy(&source, &destination).is_err());
        assert_eq!(
            std::fs::read_to_string(destination.join("keep")).unwrap(),
            "unchanged"
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected_and_executable_state_is_hashed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let file = source.join("entry");
        std::fs::write(&file, "bytes").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let plain = copy(&source, &temp.path().join("plain")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let executable = copy(&source, &temp.path().join("executable")).unwrap();
        assert_ne!(plain.sha256().unwrap(), executable.sha256().unwrap());
        symlink(&file, source.join("link")).unwrap();
        assert!(copy(&source, &temp.path().join("refused")).is_err());
    }
}
