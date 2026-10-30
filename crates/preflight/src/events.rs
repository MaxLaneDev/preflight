//! Durable completion facts; push notifications are hints to resume by cursor.
use crate::{db::Db, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    #[serde(default)]
    pub after: i64,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
fn default_limit() -> usize {
    50
}
pub fn uri(project: &str) -> String {
    format!("preflight://projects/{project}/completions")
}
pub fn project(uri: &str) -> Result<&str> {
    let project = uri
        .strip_prefix("preflight://projects/")
        .and_then(|v| v.strip_suffix("/completions"))
        .ok_or_else(|| invalid("unknown completion resource URI"))?;
    ensure!(
        !project.is_empty()
            && project.len() <= 64
            && project
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        invalid("invalid project resource URI")
    );
    Ok(project)
}
impl Db {
    pub fn completion_cursor(&self, project: &str) -> Result<i64> {
        self.project(project)?;
        Ok(self.connection()?.query_row(
            "SELECT COALESCE(MAX(cursor),0) FROM completion_events WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?)
    }
    pub fn completion_events(&self, q: &Query) -> Result<Value> {
        ensure!(
            q.after >= 0 && (1..=100).contains(&q.limit),
            invalid("after must be nonnegative and limit must be 1..100")
        );
        self.project(&q.project_id)?;
        let c = self.connection()?;
        let mut stmt=c.prepare("SELECT cursor,run_id,status,finished_at,historical FROM completion_events WHERE project_id=?1 AND cursor>?2 ORDER BY cursor LIMIT ?3")?;
        let mut rows=stmt.query_map(params![q.project_id,q.after,q.limit+1],|r|Ok(json!({"cursor":r.get::<_,i64>(0)?,"run_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"finished_at":r.get::<_,i64>(3)?,"historical":r.get::<_,bool>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > q.limit;
        rows.truncate(q.limit);
        let next = rows
            .last()
            .and_then(|v| v["cursor"].as_i64())
            .unwrap_or(q.after);
        Ok(
            json!({"project_id":q.project_id,"events":rows,"next_cursor":next,"has_more":more,"resource_uri":uri(&q.project_id)}),
        )
    }
    pub fn completion_tail(&self, project: &str) -> Result<Value> {
        self.project(project)?;
        let first:i64=self.connection()?.query_row("SELECT COALESCE(MIN(cursor),1) FROM (SELECT cursor FROM completion_events WHERE project_id=?1 ORDER BY cursor DESC LIMIT 100)",[project],|r|r.get(0))?;
        let mut value = self.completion_events(&Query {
            project_id: project.into(),
            after: first - 1,
            limit: 100,
        })?;
        value["view"] = "latest_100; use events_read after=0 for complete replay".into();
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandSpec, RunState, StartBuild};
    #[test]
    fn durable_ordered_project_events_and_recovery_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite3");
        let db = Db::new(&path).unwrap();
        let project = db.register("p", dir.path()).unwrap();
        let other_root = dir.path().join("other");
        std::fs::create_dir(&other_root).unwrap();
        let other = db.register("other", &other_root).unwrap();
        let request = StartBuild {
            project_id: project.id.clone(),
            command: CommandSpec {
                program: "tool".into(),
                args: vec![],
                timeout_seconds: 5,
            },
        };
        for id in ["success", "cancelled", "interrupted"] {
            db.insert_run(id, &request, dir.path()).unwrap();
        }
        db.transition("success", RunState::Succeeded, Some(0), None, None)
            .unwrap();
        db.transition(
            "cancelled",
            RunState::Cancelled,
            None,
            Some("user"),
            Some("requested"),
        )
        .unwrap();
        assert!(
            db.transition("success", RunState::Succeeded, Some(0), None, None)
                .is_err()
        );
        let first = db
            .completion_events(&Query {
                project_id: project.id.clone(),
                after: 0,
                limit: 1,
            })
            .unwrap();
        assert_eq!(first["events"][0]["run_id"], "success");
        assert_eq!(first["has_more"], true);
        let cursor = first["next_cursor"].as_i64().unwrap();
        db.reconcile().unwrap();
        db.reconcile().unwrap();
        drop(db);
        let db = Db::new(&path).unwrap();
        let tail = db
            .completion_events(&Query {
                project_id: project.id.clone(),
                after: cursor,
                limit: 100,
            })
            .unwrap();
        assert_eq!(tail["events"].as_array().unwrap().len(), 2);
        assert_eq!(tail["events"][0]["run_id"], "cancelled");
        assert_eq!(tail["events"][1]["status"], "infrastructure_error");
        assert_eq!(tail["events"][1]["historical"], false);
        let empty = db
            .completion_events(&Query {
                project_id: other.id,
                after: 0,
                limit: 100,
            })
            .unwrap();
        assert_eq!(empty["events"], json!([]));
        assert!(
            db.completion_events(&Query {
                project_id: project.id.clone(),
                after: -1,
                limit: 100
            })
            .is_err()
        );
        assert!(
            db.completion_events(&Query {
                project_id: project.id.clone(),
                after: 0,
                limit: 101
            })
            .is_err()
        );
        assert!(super::project("preflight://projects/../completions").is_err());
        assert_eq!(super::project(&uri(&project.id)).unwrap(), project.id);
    }
}
