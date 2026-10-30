use crate::model::{Run, invalid};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub kind: String,
    pub sha256: Option<String>,
    pub size: u64,
    pub executable: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    pub version: u32,
    pub tree_sha256: String,
    pub configuration_sha256: String,
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputQuery {
    pub run_id: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareQuery {
    pub run_id: String,
    pub baseline_id: String,
    #[serde(default)]
    pub offset: usize,
}
pub fn capture(root: &Path, run: &Run) -> Result<Manifest> {
    fn visit(root: &Path, dir: &Path, entries: &mut BTreeMap<String, Entry>) -> Result<()> {
        for item in std::fs::read_dir(dir)? {
            let path = item?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(!metadata.is_symlink(), "input snapshot contains symlink");
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("input path is not UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let mut entry = Entry {
                kind: "directory".into(),
                sha256: None,
                size: 0,
                executable: false,
            };
            if metadata.is_file() {
                entry.kind = "file".into();
                let mut file = std::fs::File::open(&path)?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    entry.size += n as u64;
                    ensure!(
                        entry.size <= 128 * 1024 * 1024,
                        "input exceeds snapshot size limit"
                    );
                    digest.update(&buffer[..n]);
                }
                entry.sha256 = Some(crate::hashing::hex(digest.finalize()));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    entry.executable = metadata.permissions().mode() & 0o111 != 0;
                }
            } else {
                ensure!(metadata.is_dir(), "input snapshot contains special file");
            }
            entries.insert(relative, entry);
            ensure!(
                entries.len() <= 20_000,
                "input snapshot exceeds entry limit"
            );
            if metadata.is_dir() {
                visit(root, &path, entries)?;
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    let configuration = json!({"pipeline":run.pipeline,"stages":run.stages.iter().map(|s| &s.definition).collect::<Vec<_>>()});
    Ok(Manifest {
        source: None,
        version: 1,
        tree_sha256: crate::hashing::hex(Sha256::digest(serde_json::to_vec(&entries)?)),
        configuration_sha256: crate::hashing::hex(Sha256::digest(serde_json::to_vec(
            &configuration,
        )?)),
        entries,
    })
}
pub fn page(m: &Manifest, offset: usize) -> Result<Value> {
    ensure!(offset <= 20_000, invalid("input cursor out of range"));
    Ok(
        json!({"version":m.version,"tree_sha256":m.tree_sha256,"configuration_sha256":m.configuration_sha256,"total":m.entries.len(),"entries":m.entries.iter().skip(offset).take(100).map(|(path,e)|json!({"path":path,"metadata":e})).collect::<Vec<_>>(),"next_cursor":(offset+100<m.entries.len()).then_some(offset+100)}),
    )
}
pub fn compare(current: &Manifest, baseline: &Manifest, offset: usize) -> Result<Value> {
    ensure!(offset <= 40_000, invalid("diff cursor out of range"));
    let paths: std::collections::BTreeSet<_> = current
        .entries
        .keys()
        .chain(baseline.entries.keys())
        .collect();
    let changes=paths.into_iter().filter_map(|path| {
        let before=baseline.entries.get(path); let after=current.entries.get(path);
        (before!=after).then(||json!({"path":path,"change":if before.is_none(){"added"}else if after.is_none(){"deleted"}else{"modified"},"before":before,"after":after}))
    }).collect::<Vec<_>>();
    Ok(
        json!({"tree_changed":current.tree_sha256!=baseline.tree_sha256,"configuration_changed":current.configuration_sha256!=baseline.configuration_sha256,"total_changes":changes.len(),"changes":changes.iter().skip(offset).take(100).collect::<Vec<_>>(),"next_cursor":(offset+100<changes.len()).then_some(offset+100)}),
    )
}
