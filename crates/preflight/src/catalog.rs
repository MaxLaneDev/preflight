//! Bounded catalog pages, shared by MCP and the optional client.
use crate::{control, engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub kind: String,
    pub project_id: Option<String>,
    #[serde(default)]
    pub query: String,
    pub after: Option<String>,
    #[serde(default = "page_size")]
    pub limit: usize,
}
fn page_size() -> usize {
    25
}
pub fn list(e: &Engine, r: &Query) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit)
            && r.query.len() <= 256
            && r.after.as_ref().is_none_or(|s| s.len() <= 512),
        invalid("invalid catalog page")
    );
    let after = r.after.as_deref().unwrap_or("");
    let q = r.query.trim().to_lowercase();
    if r.kind == "workspaces" {
        let c = e.db.connection()?;
        let mut statement =
            c.prepare("SELECT id,name,root FROM projects WHERE id>?1 ORDER BY id")?;
        let mut rows = Vec::new();
        for row in statement.query_map([after], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (id, name, root) = row?;
            if !crate::search::matches_query(&format!("{name} {id} {root}"), &q) {
                continue;
            }
            rows.push(json!({"key":id,"id":id,"name":name,"root":root}));
            if rows.len() > r.limit {
                break;
            }
        }
        return Ok(page(rows, r.limit));
    }
    ensure!(
        matches!(
            r.kind.as_str(),
            "pipelines" | "saved_pipelines" | "experiments"
        ),
        invalid("unknown catalog kind")
    );
    let id = r
        .project_id
        .as_deref()
        .ok_or_else(|| invalid("project_id required"))?;
    let project = e.db.project(id)?;
    let documents = if r.kind == "saved_pipelines" {
        "pipelines"
    } else {
        &r.kind
    };
    let hidden:std::collections::BTreeSet<String>=e.db.connection()?.prepare("SELECT target_id FROM management_trash WHERE project_id=?1 AND kind='pipeline' AND restored_at IS NULL")?.query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut rows = BTreeMap::<String, Value>::new();
    if r.kind == "pipelines" {
        let c = e.db.connection()?;
        let mut statement=c.prepare("SELECT *,printf('%020d',9223372036854775807-COALESCE(finished,0))||':'||pipeline_key AS sort_key FROM (SELECT p.pipeline_key,p.name,p.saved_name,COUNT(*) AS runs,MAX(r.id) AS latest_run,MAX(r.finished_at) AS finished FROM run_pipeline_identity p JOIN runs r ON r.id=p.run_id WHERE r.project_id=?1 AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.restored_at IS NULL AND ((t.kind='pipeline' AND t.target_id=p.pipeline_key) OR (t.kind='run' AND t.target_id=r.id))) AND preflight_contains(p.name||' '||p.pipeline_key,?3) AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=r.id) GROUP BY p.pipeline_key) WHERE sort_key>?2 ORDER BY sort_key LIMIT ?4")?;
        for row in statement.query_map(params![id,after,q,r.limit+1],|r|Ok((r.get::<_,String>(6)?,json!({"pipeline_key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"saved_name":r.get::<_,Option<String>>(2)?,"runs":r.get::<_,u64>(3)?,"latest_run":r.get::<_,String>(4)?,"finished_at":r.get::<_,Option<i64>>(5)?}))))? { let (key,value)=row?;rows.insert(key,value); }
    }
    let path = control::config_path(&project.root, &format!(".preflight/{documents}"))?;
    if path.exists() {
        // Enumerate names/revision numbers; return only a bounded page, never definition bodies.
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if control::validate_name(&name).is_err() {
                continue;
            }
            let key = if documents == "pipelines" {
                format!("saved:{name}")
            } else {
                name.clone()
            };
            if !crate::search::matches_query(&format!("{name} {key}"), &q) {
                continue;
            }
            if documents == "pipelines" && hidden.contains(&key) {
                continue;
            }
            if control::latest(&project.root, documents, &name)? == 0 {
                continue;
            }
            let sort_key = if r.kind == "pipelines" {
                let finished: Option<i64> = e.db.connection()?.query_row("SELECT MAX(r.finished_at) FROM runs r JOIN run_pipeline_identity p ON p.run_id=r.id WHERE r.project_id=?1 AND p.pipeline_key=?2 AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.kind='run' AND t.target_id=r.id AND t.restored_at IS NULL) AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=r.id)",params![id,key],|r|r.get(0))?;
                format!("{:020}:{key}", i64::MAX - finished.unwrap_or(0))
            } else {
                key.clone()
            };
            if sort_key.as_str() <= after {
                continue;
            }
            rows.entry(sort_key)
                .or_insert_with(|| json!({"name":name,"pipeline_key":key,"saved_name":name}));
            while rows.len() > r.limit + 1 {
                rows.pop_last();
            }
        }
    }
    let mut values = Vec::new();
    for (key, mut value) in rows {
        if r.kind == "experiments" || value["saved_name"].is_string() {
            let name = value["name"].as_str().unwrap_or_default();
            let revision = control::latest(&project.root, documents, name)?;
            if revision == 0 && r.kind == "experiments" {
                continue;
            }
            if revision == 0 {
                value["saved_name"] = Value::Null;
            }
            if revision > 0 {
                value["revision"] = revision.into();
            }
        }
        value["key"] = key.into();
        values.push(value);
    }
    Ok(page(values, r.limit))
}
fn page(mut items: Vec<Value>, limit: usize) -> Value {
    let more = items.len() > limit;
    items.truncate(limit);
    let next = more.then(|| items.last().unwrap()["key"].clone());
    json!({"items":items,"next_cursor":next,"has_more":more})
}
