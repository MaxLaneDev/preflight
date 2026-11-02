//! Bounded deterministic diagnostics over the durable redacted log index.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::OnceLock};
pub(crate) const FORMAT: &str = "preflight-diagnostics-v1";
pub(crate) const LINE_LIMIT: usize = 65536;
pub(crate) const GROUP_LIMIT: usize = 2000;
fn limit() -> usize {
    20
}
fn scan_bytes() -> usize {
    1024 * 1024
}
fn scan_events() -> usize {
    4096
}
fn output_bytes() -> usize {
    8192
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub run_id: String,
    pub baseline_id: Option<String>,
    pub stage_id: Option<String>,
    pub severity: Option<String>,
    pub file: Option<String>,
    #[serde(default)]
    pub query: String,
    /// Output group offset. Each request advances a bounded durable input cursor.
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default = "scan_bytes")]
    pub scan_bytes: usize,
    #[serde(default = "scan_events")]
    pub scan_events: usize,
    /// Hard JSON response budget in bytes, independently of the input scan budget.
    #[serde(default = "output_bytes")]
    pub max_bytes: usize,
}
struct Patterns {
    ansi: regex::Regex,
    located: regex::Regex,
    go: regex::Regex,
    rust: regex::Regex,
    location: regex::Regex,
    generic: regex::Regex,
    address: regex::Regex,
}
fn patterns() -> &'static Patterns {
    static VALUE: OnceLock<Patterns> = OnceLock::new();
    VALUE.get_or_init(|| Patterns {
        ansi: regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap(),
        located: regex::Regex::new(
            r"^(.+?):([0-9]+)(?::([0-9]+))?:\s*(fatal error|error|warning):\s*(.+)$",
        )
        .unwrap(),
        go: regex::Regex::new(r"^(.+\.go):([0-9]+)(?::([0-9]+))?:\s*(.+)$").unwrap(),
        rust: regex::Regex::new(r"^(error|warning)(?:\[([^\]]+)\])?:\s*(.+)$").unwrap(),
        location: regex::Regex::new(r"^\s*-->\s*(.+):([0-9]+):([0-9]+)\s*$").unwrap(),
        generic: regex::Regex::new(
            r"(?i)^(?:[a-z0-9_.+-]+:\s*)?(ERROR|WARNING|fatal|panic):\s*(.+)$",
        )
        .unwrap(),
        address: regex::Regex::new(r"\b0x[0-9a-fA-F]+\b").unwrap(),
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Diagnostic {
    pub(crate) adapter: String,
    pub(crate) severity: String,
    pub(crate) code: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) line: Option<u64>,
    pub(crate) column: Option<u64>,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) details: Vec<String>,
    pub(crate) evidence: Vec<i64>,
}
fn diagnostic(adapter: &str, severity: &str, message: &str, id: i64) -> Diagnostic {
    Diagnostic {
        adapter: adapter.into(),
        severity: if severity == "warning" {
            "warning"
        } else {
            "error"
        }
        .into(),
        code: None,
        file: None,
        line: None,
        column: None,
        message: crate::log_search::redact(message),
        details: Vec::new(),
        evidence: vec![id],
    }
}
fn parse(line: &str, id: i64) -> Option<Diagnostic> {
    let p = patterns();
    let redacted = crate::log_search::redact(line);
    let stripped = p.ansi.replace_all(&redacted, "");
    let line = stripped.trim();
    if let Ok(value) = serde_json::from_str::<Value>(line) {
        let value = if value["reason"] == "compiler-message" {
            &value["message"]
        } else {
            &value
        };
        if let (Some(level), Some(message)) = (value["level"].as_str(), value["message"].as_str())
            && matches!(level, "error" | "warning" | "fatal")
            && !message.starts_with("aborting due to")
            && !message.starts_with("could not compile")
        {
            let mut d = diagnostic("rustc-json", level, message, id);
            d.code = value["code"]["code"].as_str().map(String::from);
            if let Some(span) = value["spans"]
                .as_array()
                .and_then(|s| s.iter().find(|s| s["is_primary"] == true))
            {
                d.file = span["file_name"].as_str().map(String::from);
                d.line = span["line_start"].as_u64();
                d.column = span["column_start"].as_u64();
                if let Some(label) = span["label"].as_str() {
                    d.details
                        .push(crate::log_search::redact(label).chars().take(512).collect());
                }
            }
            if let Some(children) = value["children"].as_array() {
                for child in children.iter().filter(|c| c["level"] == "note").take(2) {
                    if let Some(message) = child["message"].as_str() {
                        d.details.push(
                            crate::log_search::redact(message)
                                .chars()
                                .take(512)
                                .collect(),
                        );
                    }
                }
            }
            return Some(d);
        }
    }
    if let Some(c) = p.located.captures(line) {
        let mut d = diagnostic("gcc-clang", &c[4], &c[5], id);
        d.file = Some(c[1].into());
        d.line = c[2].parse().ok();
        d.column = c.get(3).and_then(|v| v.as_str().parse().ok());
        return Some(d);
    }
    if let Some(c) = p.go.captures(line) {
        let mut d = diagnostic("go", "error", &c[4], id);
        d.file = Some(c[1].into());
        d.line = c[2].parse().ok();
        d.column = c.get(3).and_then(|v| v.as_str().parse().ok());
        return Some(d);
    }
    if let Some(c) = p.rust.captures(line) {
        if c[3].starts_with("aborting due to") || c[3].starts_with("could not compile") {
            return None;
        }
        let mut d = diagnostic(
            if c.get(2).is_some() {
                "rustc-text"
            } else {
                "generic"
            },
            &c[1],
            &c[3],
            id,
        );
        d.code = c.get(2).map(|v| v.as_str().into());
        return Some(d);
    }
    p.generic.captures(line).map(|c| {
        diagnostic(
            "generic",
            if c[1].eq_ignore_ascii_case("warning") {
                "warning"
            } else {
                "error"
            },
            &c[2],
            id,
        )
    })
}
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Stream {
    pub(crate) tail: String,
    pub(crate) start: i64,
    #[serde(default)]
    pub(crate) ids: Vec<i64>,
    pub(crate) overflow: bool,
    pub(crate) pending: Option<Diagnostic>,
}
pub(crate) struct Group {
    pub(crate) stage: String,
    pub(crate) sample: Diagnostic,
    pub(crate) count: usize,
    pub(crate) evidence: Vec<i64>,
}
#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) groups: BTreeMap<String, Group>,
    pub(crate) events: usize,
    pub(crate) bytes: usize,
    pub(crate) complete: bool,
    pub(crate) drained: bool,
    pub(crate) last_event: Option<i64>,
    pub(crate) omitted_lines: usize,
    pub(crate) occurrences: Vec<(String, Vec<i64>)>,
}
fn normalize_file(file: &str, root: &str, workspace: &str, stage: &str) -> String {
    let file = file.replace('\\', "/");
    let root = root.replace('\\', "/");
    let workspace = workspace.replace('\\', "/");
    for prefix in [
        format!("{workspace}/stages/{stage}/"),
        format!("{workspace}/source/"),
        format!("{root}/"),
    ] {
        if let Some(value) = file.strip_prefix(&prefix) {
            return value.trim_start_matches("./").into();
        }
    }
    file.trim_start_matches("./").into()
}
pub(crate) fn normalized_message(message: &str) -> String {
    patterns()
        .address
        .replace_all(message, "<address>")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
impl Scan {
    pub(crate) fn emit(&mut self, stage: &str, mut d: Diagnostic, root: &str, workspace: &str) {
        if let Some(file) = &d.file {
            d.file = Some(normalize_file(file, root, workspace, stage));
        }
        let normalized = normalized_message(&d.message);
        // Adapter spellings and moving line numbers do not split the same issue.
        let key = format!(
            "diag:v1:{}",
            crate::hashing::hex(Sha256::digest(
                serde_json::to_vec(&(stage, &d.severity, &d.code, &d.file, normalized)).unwrap()
            ))
        );
        self.occurrences.push((key.clone(), d.evidence.clone()));
        if let Some(group) = self.groups.get_mut(&key) {
            group.count += 1;
            for id in d.evidence {
                if group.evidence.len() < 4 && !group.evidence.contains(&id) {
                    group.evidence.push(id);
                }
            }
        } else if self.groups.len() < GROUP_LIMIT {
            self.groups.insert(
                key,
                Group {
                    stage: stage.into(),
                    count: 1,
                    evidence: d.evidence.clone(),
                    sample: d,
                },
            );
        } else {
            self.omitted_lines += 1;
        }
    }
    pub(crate) fn line(
        &mut self,
        stage: &str,
        stream: &mut Stream,
        line: &str,
        source: &[i64],
        root: &str,
        workspace: &str,
    ) {
        let Some(&id) = source.first() else {
            return;
        };
        if let Some(mut d) = parse(line, id) {
            d.evidence = source.to_vec();
            if let Some(old) = stream.pending.take() {
                self.emit(stage, old, root, workspace);
            }
            if d.file.is_none() && (d.adapter == "rustc-text" || d.adapter == "generic") {
                stream.pending = Some(d);
            } else {
                self.emit(stage, d, root, workspace);
            }
        } else if let Some(c) = patterns()
            .location
            .captures(&patterns().ansi.replace_all(line, ""))
            && let Some(mut d) = stream.pending.take()
        {
            d.adapter = "rustc-text".into();
            d.file = Some(c[1].into());
            d.line = c[2].parse().ok();
            d.column = c[3].parse().ok();
            for id in source {
                if !d.evidence.contains(id) {
                    d.evidence.push(*id);
                }
            }
            self.emit(stage, d, root, workspace);
        }
    }
}
fn scan(
    e: &Engine,
    r: &Query,
    id: &str,
    terminal: bool,
    root: &str,
    workspace: &str,
) -> Result<Scan> {
    crate::diagnostic_index::advance(
        e,
        id,
        r.scan_bytes,
        r.scan_events,
        terminal,
        root,
        workspace,
    )?;
    crate::diagnostic_index::snapshot(e, r, id, terminal)
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
pub fn report(e: &Engine, r: &Query) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit) && r.offset <= 10000,
        invalid("limit must be 1..100; offset 0..10000")
    );
    ensure!(
        (1024..=4 * 1024 * 1024).contains(&r.scan_bytes) && (1..=10000).contains(&r.scan_events),
        invalid("scan_bytes must be 1024..4194304; scan_events 1..10000")
    );
    ensure!(
        (4096..=65536).contains(&r.max_bytes),
        invalid("max_bytes must be 4096..65536")
    );
    if let Some(severity) = &r.severity {
        ensure!(
            matches!(severity.as_str(), "error" | "warning"),
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
        r.query.len() <= 512 && r.query.split_whitespace().count() <= 16,
        invalid("query is limited to 512 bytes / 16 literal AND terms")
    );
    if let Some(stage) = &r.stage_id {
        crate::pipeline::identifier(stage)?;
    }
    let project = e.db.project(&r.project_id)?;
    let run = e.db.run(&r.run_id)?;
    ensure!(run.project_id == r.project_id, Fault::NotFound);
    let baseline_run = if let Some(id) = &r.baseline_id {
        let baseline = e.db.run(id)?;
        ensure!(baseline.project_id == r.project_id, Fault::NotFound);
        Some(baseline)
    } else {
        None
    };
    let current = scan(
        e,
        r,
        &r.run_id,
        run.status.terminal(),
        &project.root.to_string_lossy(),
        &run.workspace.to_string_lossy(),
    )?;
    let baseline = baseline_run
        .as_ref()
        .map(|run| {
            scan(
                e,
                r,
                &run.id,
                run.status.terminal(),
                &project.root.to_string_lossy(),
                &run.workspace.to_string_lossy(),
            )
        })
        .transpose()?;
    let indexes_complete = current.complete && baseline.as_ref().is_some_and(|s| s.complete);
    let completed_stage = |run: &crate::model::Run, stage: &str| {
        run.stages.iter().any(|s| {
            s.definition.id == stage
                && matches!(
                    s.status,
                    crate::model::RunState::Succeeded | crate::model::RunState::Failed
                )
        })
    };
    let mut comparison_complete = indexes_complete;
    let mut groups = Vec::new();
    for (key, group) in &current.groups {
        let previous = baseline.as_ref().and_then(|s| s.groups.get(key));
        let state = if baseline.is_none() {
            "observed"
        } else if previous.is_some() {
            "recurring"
        } else if indexes_complete
            && baseline_run
                .as_ref()
                .is_some_and(|run| completed_stage(run, &group.stage))
        {
            "new"
        } else {
            "not_observed_in_baseline_window"
        };
        groups.push((key, group, state, previous.map(|g| g.count)));
    }
    if let Some(baseline) = &baseline {
        for (key, group) in &baseline.groups {
            if !current.groups.contains_key(key) {
                groups.push((
                    key,
                    group,
                    if indexes_complete && completed_stage(&run, &group.stage) {
                        "fixed"
                    } else {
                        "not_observed_in_current_window"
                    },
                    Some(group.count),
                ));
            }
        }
    }
    if groups
        .iter()
        .any(|(_, _, state, _)| state.starts_with("not_observed_"))
    {
        comparison_complete = false;
    }
    groups.sort_by(|a, b| {
        a.1.sample
            .severity
            .cmp(&b.1.sample.severity)
            .then(b.1.count.cmp(&a.1.count))
            .then(a.0.cmp(b.0))
    });
    let mut counts = BTreeMap::<&str, usize>::new();
    for (_, _, state, _) in &groups {
        *counts.entry(state).or_default() += 1;
    }
    let info = |s: &Scan| json!({"complete":s.complete,"drained":s.drained,"events":s.events,"bytes":s.bytes,"last_event":s.last_event,"omitted_lines":s.omitted_lines,"groups":s.groups.len()});
    let mut response = json!({"format":FORMAT,"project_id":r.project_id,"run_id":r.run_id,"run_status":run.status,"baseline_id":r.baseline_id,"baseline_status":baseline_run.as_ref().map(|r|r.status.as_str()),"current_scan":info(&current),"baseline_scan":baseline.as_ref().map(info),"comparison_complete":comparison_complete,"group_counts":counts,"groups":[],"next_offset":null,"coverage":"recognized compiler-style diagnostics in an incrementally indexed redacted log; incomplete indexes never prove new/fixed; not test counts or build success","classification_basis":"observed diagnostic changes, not proof of a source bug fix; missing/unexecuted stages are unknown","fingerprint":"severity/code/stage/normalized file/message; excludes line/column; v1","evidence_tool":"evidence_read"});
    let mut selected = Vec::new();
    let mut next = r.offset;
    for (key, group, state, previous) in groups.iter().skip(r.offset).take(r.limit) {
        let d = &group.sample;
        let from_baseline = *state == "fixed" || *state == "not_observed_in_current_window";
        let value = json!({"fingerprint":key,"stage_id":group.stage,"state":state,"adapter":d.adapter,"severity":d.severity,"code":d.code,"file":d.file.as_ref().map(|s|cut(s,160)),"line":d.line,"column":d.column,"message":cut(&d.message,256),"details":d.details.iter().take(3).map(|s|cut(s,256)).collect::<Vec<_>>(),"sample_truncated":d.message.chars().count()>256||d.file.as_ref().is_some_and(|f|f.chars().count()>160),"occurrences":if from_baseline{0}else{group.count},"baseline_occurrences":previous,"evidence_run":if from_baseline{r.baseline_id.as_deref().unwrap()}else{&r.run_id},"evidence_ids":group.evidence.iter().take(4).map(|id|format!("log:{id}")).collect::<Vec<_>>()});
        selected.push(value);
        response["groups"] = json!(selected);
        response["next_offset"] = json!(next + 1);
        if serde_json::to_vec(&response)?.len() > r.max_bytes {
            selected.pop();
            response["groups"] = json!(selected);
            break;
        }
        next += 1;
    }
    ensure!(
        !selected.is_empty() || r.offset >= groups.len(),
        invalid("max_bytes too small for one diagnostic group; increase the response budget")
    );
    response["next_offset"] = if next < groups.len() {
        json!(next)
    } else {
        Value::Null
    };
    ensure!(
        serde_json::to_vec(&response)?.len() <= r.max_bytes,
        invalid("diagnostic response budget too small")
    );
    Ok(response)
}
pub async fn run_indexer(e: std::sync::Arc<Engine>) {
    crate::diagnostic_index::run(e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiler_formats_and_stable_location_normalization() {
        assert_eq!(
            normalize_file(
                r"C:\runs\one/stages/build/src/main.rs",
                r"C:\project",
                r"C:\runs\one",
                "build"
            ),
            "src/main.rs"
        );
        let clang = parse(
            "C:\\src\\main.cpp:12:3: error: unknown type name 'thing'",
            1,
        )
        .unwrap();
        assert_eq!(clang.line, Some(12));
        assert_eq!(clang.column, Some(3));
        let go = parse("main.go:8:2: undefined: missing", 2).unwrap();
        assert_eq!(go.adapter, "go");
        let rust=parse(r#"{"reason":"compiler-message","message":{"level":"error","message":"bad type","code":{"code":"E0308"},"spans":[{"is_primary":true,"file_name":"src/main.rs","line_start":3,"column_start":4}]}}"#,3).unwrap();
        assert_eq!(rust.code.as_deref(), Some("E0308"));
        assert_eq!(rust.line, Some(3));
        let mut scan = Scan::default();
        let mut stream = Stream::default();
        scan.line(
            "build",
            &mut stream,
            "\u{1b}[31merror[E0308]: bad type\u{1b}[0m",
            &[4],
            "/project",
            "/runs/one",
        );
        scan.line(
            "build",
            &mut stream,
            " --> /runs/one/stages/build/src/main.rs:99:1",
            &[5],
            "/project",
            "/runs/one",
        );
        scan.emit("build", rust, "/project", "/runs/one");
        assert_eq!(scan.groups.len(), 1);
        assert_eq!(scan.groups.values().next().unwrap().count, 2);
        assert!(parse("error: could not compile `x` due to previous error", 6).is_none());
        assert!(parse("test result: ok. 50 passed", 7).is_none());
    }
}
