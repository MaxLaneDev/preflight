//! Bounded, read-only file-length accounting; rejects observed storage symlinks.
//! Live best effort: filesystem mutations during scanning are not an atomic snapshot.
use crate::{engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path, sync::Arc};

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    RunWorkspaces,
    StageWorkspaces,
    ModuleCopies,
    Artifacts,
    Logs,
    SourceSnapshots,
    CacheArtifacts,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    /// Exact cache pipeline ID, matching storage_gc_preview. Not a UI pipeline key.
    pub pipeline_id: Option<String>,
    pub category: Category,
    pub before: Option<String>,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default = "budget")]
    pub max_entries: usize,
}
fn limit() -> usize {
    20
}
fn budget() -> usize {
    10_000
}

// Check every parent first; symlink_metadata on the final path alone would follow ancestors.
fn checked_path(base: &Path, relative: &Path) -> Result<bool> {
    crate::storage_gc::directory(base)?;
    let mut path = base.to_path_buf();
    for component in relative.components() {
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "invalid storage component"
        );
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(m) => ensure!(
                m.is_dir() && !m.is_symlink(),
                "storage directory is linked or not a directory"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}
fn measure(base: &Path, relative: &Path, remaining: &mut usize) -> Value {
    let exists = match checked_path(base, relative) {
        Ok(v) => v,
        Err(e) => return json!({"complete":false,"observed_file_bytes":0,"reason":e.to_string()}),
    };
    if !exists {
        return json!({"complete":true,"observed_file_bytes":0,"state":"absent"});
    }
    let mut queue = VecDeque::from([base.join(relative)]);
    let mut bytes = 0u64;
    let mut files = 0usize;
    let result = (|| -> Result<()> {
        while let Some(path) = queue.pop_front() {
            ensure!(*remaining > 0, "scan budget exhausted");
            *remaining -= 1;
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.is_symlink() && (metadata.is_dir() || metadata.is_file()),
                "linked or special entry; accounting incomplete"
            );
            if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| invalid("byte count overflow"))?;
                files += 1;
            } else {
                // Bound queued names as well as processed entries, even for a very wide directory.
                for entry in std::fs::read_dir(&path)? {
                    ensure!(queue.len() < *remaining, "scan budget exhausted");
                    queue.push_back(entry?.path());
                }
            }
        }
        Ok(())
    })();
    json!({"complete":result.is_ok(),"observed_file_bytes":bytes,"files":files,"state":"present","reason":result.err().map(|e|e.to_string())})
}
fn read(e: &Engine, r: Query) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    ensure!(
        (1..=200_000).contains(&r.max_entries),
        invalid("max_entries must be 1..200000")
    );
    e.db.project(&r.project_id)?;
    uuid::Uuid::parse_str(&r.project_id)?;
    if let Some(p) = &r.pipeline_id {
        crate::pipeline::identifier(p)?;
    }
    ensure!(
        !matches!(r.category, Category::RunWorkspaces) || r.pipeline_id.is_none(),
        invalid("run_workspaces requires project scope; use stage_workspaces for a cache pipeline")
    );
    let (category, sql) = match r.category {
        Category::Logs | Category::SourceSnapshots => (
            if matches!(r.category, Category::Logs) {
                "logs"
            } else {
                "source_snapshots"
            },
            "SELECT r.id,'',r.status FROM runs r WHERE r.project_id=?1 AND (?2 IS NULL OR EXISTS(SELECT 1 FROM stage_runs s WHERE s.run_id=r.id AND json_extract(s.definition_json,'$.cache.pipeline_id')=?2)) AND (?3 IS NULL OR r.id<?3) ORDER BY r.id DESC LIMIT ?4",
        ),
        Category::RunWorkspaces => (
            "run_workspaces",
            "SELECT id,'',status FROM runs WHERE project_id=?1 AND (?2 IS NULL) AND (?3 IS NULL OR id<?3) ORDER BY id DESC LIMIT ?4",
        ),
        Category::StageWorkspaces => (
            "stage_workspaces",
            "SELECT r.id,s.name,r.status FROM runs r JOIN stage_runs s ON s.run_id=r.id WHERE r.project_id=?1 AND r.pipeline_json IS NOT NULL AND (?2 IS NULL OR json_extract(s.definition_json,'$.cache.pipeline_id')=?2) AND (?3 IS NULL OR r.id||'/'||s.name<?3) ORDER BY r.id DESC,s.name DESC LIMIT ?4",
        ),
        Category::CacheArtifacts => (
            "cache_artifacts",
            "SELECT r.id,s.name,r.status FROM runs r JOIN stage_runs s ON s.run_id=r.id WHERE r.project_id=?1 AND EXISTS(SELECT 1 FROM stage_cache c WHERE c.producer_run=r.id AND c.producer_stage=s.name AND c.state='active') AND (?2 IS NULL OR json_extract(s.definition_json,'$.cache.pipeline_id')=?2) AND (?3 IS NULL OR r.id||'/'||s.name<?3) ORDER BY r.id DESC,s.name DESC LIMIT ?4",
        ),
        Category::Artifacts => (
            "artifacts",
            "SELECT r.id,s.name,r.status FROM runs r JOIN stage_runs s ON s.run_id=r.id WHERE r.project_id=?1 AND EXISTS(SELECT 1 FROM artifacts a WHERE a.run_id=r.id AND a.stage_id=s.name) AND (?2 IS NULL OR json_extract(s.definition_json,'$.cache.pipeline_id')=?2) AND (?3 IS NULL OR r.id||'/'||s.name<?3) ORDER BY r.id DESC,s.name DESC LIMIT ?4",
        ),
        Category::ModuleCopies => (
            "module_copies",
            "SELECT m.sha256,'','published' FROM modules m WHERE m.project_id=?1 AND (?3 IS NULL OR m.sha256<?3) AND (?2 IS NULL OR EXISTS(SELECT 1 FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN stage_runs s ON s.run_id=a.run_id AND s.name=a.stage_id WHERE r.project_id=m.project_id AND a.sha256=m.sha256 AND json_extract(s.definition_json,'$.cache.pipeline_id')=?2)) ORDER BY m.sha256 DESC LIMIT ?4",
        ),
    };
    let rows = {
        let c = e.db.connection()?;
        let mut q = c.prepare(sql)?;
        q.query_map(
            params![r.project_id, r.pipeline_id, r.before, r.limit + 1],
            |v| {
                Ok((
                    v.get::<_, String>(0)?,
                    v.get::<_, String>(1)?,
                    v.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > r.limit;
    let mut remaining = r.max_entries;
    let mut items = Vec::new();
    let mut cursor = None;
    let mut total = 0u64;
    let mut complete = true;
    for (id, stage, status) in rows.into_iter().take(r.limit) {
        let key = if stage.is_empty() {
            id.clone()
        } else {
            format!("{id}/{stage}")
        };
        cursor = Some(key.clone());
        if matches!(r.category, Category::Logs) {
            let c = e.db.connection()?;
            let bytes:u64=c.query_row("SELECT COALESCE((SELECT SUM(length(CAST(text AS BLOB))) FROM log_events WHERE run_id=?1),0)+COALESCE((SELECT SUM(length(payload)) FROM log_chunks WHERE run_id=?1),0)+COALESCE((SELECT SUM(length(CAST(f.text AS BLOB))) FROM log_search f JOIN log_events l ON l.id=f.rowid WHERE l.run_id=?1),0)",[&id],|v|v.get(0))?;
            if remaining == 0 {
                complete = false;
                break;
            }
            remaining -= 1;
            total += bytes;
            items.push(json!({"id":key,"observed_file_bytes":bytes,"complete":true,"state":if crate::evidence_expiry::expired(&c,&id,crate::evidence_expiry::Category::Logs)?{"logs_expired"}else{"logical_payload"},"accounting":"SQLite logical payload bytes, not allocated disk space"}));
            continue;
        }
        let relative = match r.category {
            Category::ModuleCopies => {
                crate::modules::validate_digest(&id)?;
                std::path::PathBuf::from("modules")
                    .join(&r.project_id)
                    .join(&id)
            }
            _ => {
                uuid::Uuid::parse_str(&id)?;
                let root = std::path::PathBuf::from("workspaces").join(&id);
                if matches!(r.category, Category::SourceSnapshots) {
                    root.join("source")
                } else if stage.is_empty() {
                    root
                } else {
                    crate::pipeline::identifier(&stage)?;
                    root.join(
                        if matches!(r.category, Category::Artifacts | Category::CacheArtifacts) {
                            "artifacts"
                        } else {
                            "stages"
                        },
                    )
                    .join(&stage)
                }
            }
        };
        let mut measured = measure(&e.config.data_dir, &relative, &mut remaining);
        let active = matches!(
            status.as_str(),
            "queued" | "preparing" | "running" | "collecting"
        );
        measured["id"] = json!(key);
        measured["path"] = json!(e.config.data_dir.join(relative));
        measured["active_run"] = json!(active);
        complete &= measured["complete"] == true && !active;
        total = total
            .checked_add(measured["observed_file_bytes"].as_u64().unwrap_or(0))
            .ok_or_else(|| invalid("byte count overflow"))?;
        items.push(measured);
    }
    Ok(
        json!({"project_id":r.project_id,"pipeline_id":r.pipeline_id,"category":category,"entries":items,"page_file_bytes":total,"page_complete":complete,"entries_scanned":r.max_entries-remaining,"next_cursor":if more{cursor}else{None},"accounting":"regular-file lengths, or log raw/compressed/redacted-index payload bytes; not allocated disk bytes; live best effort","scope":"known database objects only; run_workspaces includes sources/stages/artifacts and overlaps stage_workspaces; module copies may be shared across pipelines","excluded":["SQLite/WAL allocated pages and normalized diagnostic index overhead","worker transfer staging","GC quarantine","toolchain and SDK external paths","unregistered/orphan storage"]}),
    )
}
pub async fn usage(e: Arc<Engine>, r: Query) -> Result<Value> {
    tokio::task::spawn_blocking(move || read(&e, r)).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measures_nested_files_and_absence() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("run/nested")).unwrap();
        std::fs::write(temp.path().join("run/a"), b"abc").unwrap();
        std::fs::write(temp.path().join("run/nested/b"), b"12345").unwrap();
        let value = measure(temp.path(), Path::new("run"), &mut 100);
        assert_eq!(value["complete"], true);
        assert_eq!(value["observed_file_bytes"], 8);
        assert_eq!(value["files"], 2);
        assert_eq!(
            measure(temp.path(), Path::new("missing"), &mut 100)["state"],
            "absent"
        );
    }
    #[test]
    fn budget_exhaustion_is_not_a_complete_total() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("run")).unwrap();
        for i in 0..10 {
            std::fs::write(temp.path().join(format!("run/{i}")), b"abc").unwrap();
        }
        let mut budget = 3;
        let value = measure(temp.path(), Path::new("run"), &mut budget);
        assert_eq!(value["complete"], false);
        assert!(budget <= 3);
        assert_eq!(value["reason"], "scan budget exhausted");
    }
    #[test]
    fn rejects_parent_traversal() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            measure(temp.path(), Path::new("../outside"), &mut 100)["complete"],
            false
        );
    }
    #[cfg(unix)]
    #[test]
    fn rejects_linked_ancestors_and_entries() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("nested")).unwrap();
        std::fs::write(outside.path().join("secret"), b"not storage").unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("linked")).unwrap();
        let v = measure(temp.path(), Path::new("linked/nested"), &mut 100);
        assert_eq!(v["complete"], false);
        std::fs::create_dir(temp.path().join("run")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), temp.path().join("run/link"))
            .unwrap();
        let v = measure(temp.path(), Path::new("run"), &mut 100);
        assert_eq!(v["complete"], false);
        assert_eq!(v["observed_file_bytes"], 0);
    }
}
