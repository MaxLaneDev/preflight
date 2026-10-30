//! Retrieval context over the durable, redacted SQLite FTS5 index.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
const KIND: &str = "COALESCE(json_extract(s.definition_json,'$.testing.kind'),CASE WHEN json_extract(s.definition_json,'$.fixtures') IS NOT NULL THEN 'fixture' WHEN json_extract(s.definition_json,'$.kind')='test' THEN 'custom' ELSE 'build' END)";
fn limit() -> usize {
    20
}
fn budget() -> usize {
    6000
}
fn neighbors() -> usize {
    2
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub project_id: String,
    #[serde(default)]
    pub query: String,
    pub run_id: Option<String>,
    pub batch_id: Option<String>,
    pub git_sha: Option<String>,
    pub severity: Option<String>,
    pub file: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub stage_id: Option<String>,
    pub test_id: Option<String>,
    pub kind: Option<String>,
    pub stream: Option<String>,
    pub before: Option<i64>,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default = "budget")]
    pub max_chars: usize,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Read {
    pub project_id: String,
    pub evidence_id: String,
    #[serde(default = "neighbors")]
    pub neighbors: usize,
    pub offset: Option<usize>,
    pub max_bytes: Option<usize>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub project_id: String,
}
fn expression(query: &str) -> Result<String> {
    ensure!(query.len() <= 512, invalid("query exceeds 512 bytes"));
    let words = query.split_whitespace().collect::<Vec<_>>();
    ensure!(words.len() <= 16, invalid("at most 16 literal terms"));
    Ok(words
        .iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND "))
}
pub fn search(e: &Engine, r: &Search) -> Result<Value> {
    let project = e.db.project(&r.project_id)?;
    ensure!(
        r.run_id.is_none() || r.batch_id.is_none() || r.run_id == r.batch_id,
        invalid("run_id and batch_id must name the same run")
    );
    if let Some(sha) = &r.git_sha {
        ensure!(
            matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            invalid("git_sha must be an exact 40- or 64-digit hexadecimal commit")
        );
    }
    if let Some(level) = &r.severity {
        ensure!(
            matches!(level.as_str(), "error" | "warning"),
            invalid("severity must be error or warning")
        );
    }
    if let Some(file) = &r.file {
        ensure!(
            !file.is_empty() && file.len() <= 1024,
            invalid("file must be 1..1024 bytes")
        );
    }
    ensure!(
        r.from_ms.is_none_or(|v| v >= 0)
            && r.to_ms.is_none_or(|v| v >= 0)
            && !matches!((r.from_ms,r.to_ms),(Some(a),Some(b)) if a>=b),
        invalid("time interval requires 0 <= from_ms < to_ms (exclusive)")
    );
    let run_id = r.run_id.as_ref().or(r.batch_id.as_ref());
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    ensure!(
        (1024..=32768).contains(&r.max_chars),
        invalid("max_chars must be 1024..32768")
    );
    for value in [&r.stage_id, &r.test_id, &r.kind, &r.stream]
        .into_iter()
        .flatten()
    {
        ensure!(
            !value.is_empty() && value.len() <= 256,
            invalid("bounded evidence filter required")
        );
    }
    if let Some(id) = run_id {
        let run = e.db.run(id)?;
        ensure!(run.project_id == r.project_id, Fault::NotFound);
        crate::evidence_expiry::available(
            &*e.db.connection()?,
            id,
            crate::evidence_expiry::Category::Logs,
        )?;
        if r.severity.is_some() || r.file.is_some() {
            crate::diagnostic_index::advance(
                e,
                id,
                262144,
                1024,
                run.status.terminal(),
                &project.root.to_string_lossy(),
                &run.workspace.to_string_lossy(),
            )?;
        }
        if let Some(test) = &r.test_id {
            annotate_native_markers(e, &run, r.stage_id.as_deref(), test)?;
        }
    }
    let expression = expression(&r.query)?;
    let predicate = if expression.is_empty() {
        "?1=''"
    } else {
        "log_search MATCH ?1"
    };
    let sql = format!(
        "SELECT l.id,l.run_id,l.stage_id,l.stream,l.timestamp,l.test_id,{KIND},snippet(log_search,0,'','',' … ',48),p.pipeline_key,p.name,l.ingested_at FROM log_search JOIN log_events l ON l.id=log_search.rowid JOIN runs r ON r.id=l.run_id LEFT JOIN stage_runs s ON s.run_id=l.run_id AND s.name=l.stage_id LEFT JOIN run_pipeline_identity p ON p.run_id=l.run_id WHERE {predicate} AND r.project_id=?2 AND (?3 IS NULL OR l.run_id=?3) AND (?4 IS NULL OR l.stage_id=?4) AND (?5 IS NULL OR l.test_id=?5) AND (?6 IS NULL OR {KIND}=?6) AND (?7 IS NULL OR l.stream=?7) AND (?8 IS NULL OR l.id<?8) AND (?10 IS NULL OR l.timestamp>=?10) AND (?11 IS NULL OR l.timestamp<?11) AND (?12 IS NULL OR EXISTS(SELECT 1 FROM ci_runs ci WHERE ci.run_id=r.id AND lower(ci.git_sha)=lower(?12)) OR EXISTS(SELECT 1 FROM run_inputs i,json_tree(i.manifest_json,'$.source') j WHERE i.run_id=r.id AND j.key='resolved_commit' AND lower(j.value)=lower(?12))) AND ((?13 IS NULL AND ?14 IS NULL) OR EXISTS(SELECT 1 FROM diagnostic_evidence de JOIN diagnostics d ON d.id=de.diagnostic_id WHERE de.event_id=l.id AND d.run_id=l.run_id AND (?13 IS NULL OR d.severity=?13) AND (?14 IS NULL OR d.file=?14))) ORDER BY l.id DESC LIMIT ?9"
    );
    let c = e.db.connection()?;
    let mut statement = c.prepare(&sql)?;
    let mut rows=statement.query_map(params![expression,r.project_id,run_id,r.stage_id,r.test_id,r.kind,r.stream,r.before,r.limit+1,r.from_ms,r.to_ms,r.git_sha,r.severity,r.file.as_ref().map(|f|f.replace('\\',"/").trim_start_matches("./").to_string())],|row|{let id:i64=row.get(0)?;let excerpt:String=row.get(7)?;Ok(json!({"evidence_id":format!("log:{id}"),"cursor":id,"run_id":row.get::<_,String>(1)?,"stage_id":row.get::<_,String>(2)?,"stream":row.get::<_,String>(3)?,"timestamp":row.get::<_,i64>(4)?,"ingested_at":row.get::<_,Option<i64>>(10)?,"test_id":row.get::<_,Option<String>>(5)?,"kind":row.get::<_,String>(6)?,"excerpt":excerpt.chars().take(1024).collect::<String>(),"excerpt_truncated":excerpt.chars().count()>1024||excerpt.contains(" … "),"pipeline_key":row.get::<_,Option<String>>(8)?,"pipeline_name":row.get::<_,Option<String>>(9)?}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > r.limit;
    rows.truncate(r.limit);
    let next = if more {
        rows.last().map(|r| r["cursor"].clone())
    } else {
        None
    };
    let diagnostic_complete = if r.severity.is_some() || r.file.is_some() {
        Some(!c.query_row("SELECT EXISTS(SELECT 1 FROM runs r LEFT JOIN diagnostic_scans s ON s.run_id=r.id WHERE r.project_id=?1 AND (?2 IS NULL OR r.id=?2) AND EXISTS(SELECT 1 FROM log_events l WHERE l.run_id=r.id) AND (r.finished_at IS NULL OR s.run_id IS NULL OR s.omitted>0 OR s.terminal_flushed=0 OR s.byte_offset>=0 OR EXISTS(SELECT 1 FROM log_events l WHERE l.run_id=r.id AND l.id>s.event_id)))",params![r.project_id,run_id],|r|r.get::<_,bool>(0))?)
    } else {
        None
    };
    Ok(
        json!({"project_id":r.project_id,"matches":rows,"expired_log_runs":c.query_row("SELECT count(*) FROM evidence_expiry x JOIN runs r ON r.id=x.run_id WHERE r.project_id=?1 AND x.category='logs'",[&r.project_id],|r|r.get::<_,u64>(0))?,"next_cursor":next,"diagnostic_filter_complete":diagnostic_complete,"time_order":"server timestamps; no inferred cross-stream causal order","retrieval":"SQLite FTS5 literal AND; empty query browses scoped evidence","redaction":"credential-shaped values redacted before indexing"}),
    )
}
pub fn read(e: &Engine, r: &Read) -> Result<Value> {
    ensure!(r.neighbors <= 8, invalid("neighbors must be 0..8"));
    let id = r
        .evidence_id
        .strip_prefix("log:")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid("invalid log evidence id"))?;
    let c = e.db.connection()?;
    type Hit = (String, String, String, Option<String>, String);
    let expired: bool=c.query_row("SELECT EXISTS(SELECT 1 FROM log_events l JOIN evidence_expiry x ON x.run_id=l.run_id JOIN runs r ON r.id=l.run_id WHERE l.id=?1 AND r.project_id=?2 AND x.category='logs')",params![id,r.project_id],|r|r.get(0))?;
    ensure!(
        !expired,
        invalid("logs_expired: event identity retained; text was explicitly expired")
    );
    let hit:Option<Hit>=c.query_row("SELECT l.run_id,l.stage_id,l.stream,l.test_id,f.text FROM log_events l JOIN log_search f ON f.rowid=l.id JOIN runs r ON r.id=l.run_id WHERE l.id=?1 AND r.project_id=?2",params![id,r.project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (run, stage, stream, test, text) = hit.ok_or(Fault::NotFound)?;
    if r.offset.is_some() || r.max_bytes.is_some() {
        ensure!(
            r.neighbors == 0,
            invalid("byte windows require neighbors=0")
        );
        let start = r.offset.unwrap_or(0);
        let budget = r.max_bytes.unwrap_or(16384);
        ensure!(
            (1024..=32768).contains(&budget) && start <= text.len() && text.is_char_boundary(start),
            invalid("max_bytes must be 1024..32768; offset must be a valid UTF-8 byte boundary")
        );
        let sha = crate::hashing::hex(Sha256::digest(text.as_bytes()));
        let window = |end: usize| json!({"evidence_id":r.evidence_id,"run_id":run,"stage_id":stage,"stream":stream,"test_id":test,"sha256":sha,"offset":start,"next_offset":if end<text.len(){Some(end)}else{None},"text":&text[start..end],"representation":"redacted UTF-8 bytes; stable across raw log archival"});
        let mut low = start;
        let mut high = (start + budget).min(text.len());
        while !text.is_char_boundary(high) {
            high -= 1;
        }
        let mut best = start;
        while low <= high {
            let mut mid = low + (high - low) / 2;
            while mid > start && !text.is_char_boundary(mid) {
                mid -= 1;
            }
            if serde_json::to_vec(&window(mid))?.len() <= budget {
                best = mid;
                if mid == text.len() {
                    break;
                }
                let mut next = mid + 1;
                while next < text.len() && !text.is_char_boundary(next) {
                    next += 1;
                }
                low = next;
            } else {
                if mid == start {
                    break;
                }
                high = mid - 1;
                while high > start && !text.is_char_boundary(high) {
                    high -= 1;
                }
            }
        }
        ensure!(
            best > start || start == text.len(),
            invalid("response budget cannot contain this evidence reference")
        );
        return Ok(window(best));
    }
    let mut events = Vec::new();
    let mut remaining = 32768usize;
    for (comparison, order) in [("<=", "DESC"), (">", "ASC")] {
        let sql = format!(
            "SELECT l.id,l.timestamp,f.text FROM log_events l JOIN log_search f ON f.rowid=l.id WHERE l.run_id=?1 AND l.stage_id=?2 AND l.stream=?3 AND l.test_id IS ?4 AND l.id {comparison} ?5 ORDER BY l.id {order} LIMIT ?6"
        );
        let values = c
            .prepare(&sql)?
            .query_map(
                params![
                    run,
                    stage,
                    stream,
                    test,
                    id,
                    r.neighbors + usize::from(comparison == "<=")
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (event, timestamp, value) in values {
            let content = value.chars().take(remaining.min(16384)).collect::<String>();
            remaining = remaining.saturating_sub(content.chars().count());
            events.push(json!({"evidence_id":format!("log:{event}"),"timestamp":timestamp,"cursor":event,"text":content,"truncated":content.chars().count()<value.chars().count()}));
        }
    }
    events.sort_by_key(|r| r["cursor"].as_i64().unwrap_or_default());
    Ok(
        json!({"evidence_id":r.evidence_id,"run_id":run,"stage_id":stage,"stream":stream,"test_id":test,"sha256":crate::hashing::hex(Sha256::digest(text.as_bytes())),"events":events,"representation":"redacted index text; stable across raw log archival"}),
    )
}
pub fn context(e: &Engine, r: &Search) -> Result<Value> {
    let found = search(e, r)?;
    let mut content = String::new();
    let mut citations = Vec::new();
    let mut truncated = false;
    for hit in found["matches"].as_array().unwrap() {
        let header = format!(
            "[{}] run={} stage={} test={} stream={}\n",
            hit["evidence_id"].as_str().unwrap(),
            hit["run_id"].as_str().unwrap(),
            hit["stage_id"].as_str().unwrap(),
            hit["test_id"].as_str().unwrap_or("—"),
            hit["stream"].as_str().unwrap()
        );
        let text = format!("{}{}\n", header, hit["excerpt"].as_str().unwrap());
        let remaining = r.max_chars.saturating_sub(content.chars().count());
        if remaining == 0 {
            truncated = true;
            break;
        }
        content.extend(text.chars().take(remaining));
        truncated |= hit["excerpt_truncated"] == true;
        let mut citation = hit.clone();
        citation.as_object_mut().unwrap().remove("excerpt");
        citations.push(citation);
        if text.chars().count() > remaining {
            truncated = true;
            break;
        }
    }
    Ok(
        json!({"project_id":r.project_id,"context":content,"citations":citations,"truncated":truncated,"next_cursor":found["next_cursor"]}),
    )
}
pub fn stats(e: &Engine, r: &Project) -> Result<Value> {
    e.db.project(&r.project_id)?;
    let c = e.db.connection()?;
    let (events,runs,test_events,bytes):(u64,u64,u64,u64)=c.query_row("SELECT COUNT(*),COUNT(DISTINCT l.run_id),COALESCE(SUM(l.test_id IS NOT NULL),0),COALESCE(SUM(length(CAST(f.text AS BLOB))),0) FROM log_events l JOIN log_search f ON f.rowid=l.id JOIN runs r ON r.id=l.run_id WHERE r.project_id=?1",[&r.project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    Ok(
        json!({"project_id":r.project_id,"backend":"sqlite_fts5","events":events,"runs":runs,"test_events":test_events,"indexed_bytes":bytes,"external_service_required":false}),
    )
}

// Old native fixture logs have framework-owned case markers, including in compressed logs.
fn annotate_native_markers(
    e: &Engine,
    run: &crate::model::Run,
    stage: Option<&str>,
    test: &str,
) -> Result<()> {
    for selected in run
        .stages
        .iter()
        .filter(|s| stage.is_none_or(|id| s.definition.id == id))
    {
        let Some(action) = &selected.definition.fixtures else {
            continue;
        };
        if !action.suite.cases.iter().any(|c| c.id == test) {
            continue;
        }
        let c = e.db.connection()?;
        let markers=c.prepare("SELECT l.id,f.text FROM log_events l JOIN log_search f ON f.rowid=l.id WHERE l.run_id=?1 AND l.stage_id=?2 AND l.stream='fixture' ORDER BY l.id")?.query_map(params![run.id,selected.definition.id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let markers = markers
            .into_iter()
            .filter_map(|(id, text)| {
                text.trim()
                    .strip_prefix("case ")
                    .filter(|name| action.suite.cases.iter().any(|c| c.id == *name))
                    .map(|name| (id, name.to_owned()))
            })
            .collect::<Vec<_>>();
        if let Some(index) = markers.iter().position(|(_, name)| name == test) {
            let end = markers.get(index + 1).map(|(id, _)| *id);
            c.execute("UPDATE log_events SET test_id=?3 WHERE run_id=?1 AND stage_id=?2 AND test_id IS NULL AND id>=?4 AND (?5 IS NULL OR id<?5)",params![run.id,selected.definition.id,test,markers[index].0,end])?;
        }
    }
    Ok(())
}
