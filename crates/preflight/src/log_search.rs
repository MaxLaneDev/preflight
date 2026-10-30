//! Project-scoped FTS evidence. Search index redacts credential-shaped values.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub query: String,
    pub run_id: Option<String>,
    pub stage_id: Option<String>,
    pub before: Option<i64>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    20
}
pub(crate) fn redact(text: &str) -> String {
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            regex::Regex::new(r"(?i)(authorization:\s*(?:bearer|basic)\s+)[^\s]+").unwrap(),
            regex::Regex::new(
                r#"(?i)(\b(?:password|passwd|secret|token|api[_-]?key)\s*[:=]\s*)[^\s,;]+"#,
            )
            .unwrap(),
        ]
    });
    patterns.iter().fold(text.to_string(), |s, re| {
        re.replace_all(&s, "${1}[REDACTED]").into_owned()
    })
}
pub(crate) fn backfill(c: &rusqlite::Connection) -> Result<()> {
    let mut statement = c.prepare("SELECT id,text FROM log_events ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let text: String = row.get(1)?;
        c.execute(
            "INSERT INTO log_search(rowid,text) VALUES (?1,?2)",
            params![id, redact(&text)],
        )?;
    }
    Ok(())
}
pub fn search(e: &Engine, r: &Query) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    ensure!(
        !r.query.trim().is_empty() && r.query.len() <= 512,
        invalid("query must be 1..512 bytes")
    );
    e.db.project(&r.project_id)?;
    if let Some(id) = &r.run_id {
        ensure!(e.db.run(id)?.project_id == r.project_id, Fault::NotFound);
    }
    let words = r.query.split_whitespace().collect::<Vec<_>>();
    ensure!(words.len() <= 16, invalid("at most 16 search terms"));
    let expression = words
        .into_iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let c = e.db.connection()?;
    let mut q=c.prepare("SELECT l.id,l.run_id,l.stage_id,l.stream,l.timestamp,snippet(log_search,0,'','',' … ',48) FROM log_search JOIN log_events l ON l.id=log_search.rowid JOIN runs r ON r.id=l.run_id WHERE log_search MATCH ?1 AND r.project_id=?2 AND (?3 IS NULL OR l.run_id=?3) AND (?4 IS NULL OR l.stage_id=?4) AND (?5 IS NULL OR l.id<?5) ORDER BY l.id DESC LIMIT ?6")?;
    let mut rows=q.query_map(params![expression,r.project_id,r.run_id,r.stage_id,r.before,r.limit+1],|row|{let id:i64=row.get(0)?;let snippet:String=row.get(5)?;Ok(json!({"evidence_id":format!("log:{id}"),"cursor":id,"run_id":row.get::<_,String>(1)?,"stage_id":row.get::<_,String>(2)?,"stream":row.get::<_,String>(3)?,"timestamp":row.get::<_,i64>(4)?,"excerpt":snippet.chars().take(1024).collect::<String>(),"excerpt_truncated":snippet.chars().count()>1024}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > r.limit;
    rows.truncate(r.limit);
    let next = if more {
        rows.last().map(|v| v["cursor"].clone())
    } else {
        None
    };
    Ok(
        json!({"project_id":r.project_id,"matches":rows,"next_cursor":next,"redaction":"credential-shaped values redacted in index; not a guarantee against arbitrary secrets","query_mode":"literal terms AND; FTS operators are not interpreted"}),
    )
}
