//! Bounded, digest-verified text comparison of captured inputs, never live checkouts.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub run_id: String,
    pub baseline_id: String,
    pub path: String,
}
const LIMIT: u64 = 256 * 1024;
fn captured(e: &Engine, run: &crate::model::Run, path: &str) -> Result<Option<String>> {
    crate::evidence_expiry::available(
        &*e.db.connection()?,
        &run.id,
        crate::evidence_expiry::Category::SourceSnapshots,
    )?;
    let manifest =
        e.db.inputs(&run.id)?
            .ok_or_else(|| invalid("captured input manifest unavailable"))?;
    let Some(entry) = manifest.entries.get(path) else {
        return Ok(None);
    };
    ensure!(entry.kind == "file", invalid("input is not a regular file"));
    ensure!(
        entry.size <= LIMIT,
        invalid("text diff limited to 256 KiB per file")
    );
    let root = if run.pipeline.is_some() {
        run.workspace.join("source")
    } else {
        run.workspace.clone()
    };
    let file = crate::pipeline::contained(&root, path)?;
    let bytes = crate::fixtures::read(&file, LIMIT as usize)?;
    ensure!(
        entry.size == bytes.len() as u64
            && entry.sha256.as_deref() == Some(&crate::hashing::hex(Sha256::digest(&bytes))),
        invalid("captured source missing or modified; digest mismatch")
    );
    ensure!(
        !bytes.contains(&0),
        invalid("binary input has no text diff")
    );
    Ok(Some(
        String::from_utf8(bytes).map_err(|_| invalid("input is not UTF-8 text"))?,
    ))
}
fn unified(path: &str, before: Option<&str>, after: Option<&str>) -> String {
    let a: Vec<_> = before.unwrap_or("").split_inclusive('\n').collect();
    let b: Vec<_> = after.unwrap_or("").split_inclusive('\n').collect();
    if before.unwrap_or("") == after.unwrap_or("") {
        return String::new();
    }
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let tail = suffix.min(3);
    let old_count = a.len() - suffix + tail - start;
    let new_count = b.len() - suffix + tail - start;
    let old = if before.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".into()
    };
    let new = if after.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".into()
    };
    let mut out = format!(
        "--- {old}\n+++ {new}\n@@ -{},{} +{},{} @@\n",
        if old_count == 0 { start } else { start + 1 },
        old_count,
        if new_count == 0 { start } else { start + 1 },
        new_count
    );
    let mut line = |prefix: char, text: &str| {
        out.push(prefix);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    for text in &a[start..prefix] {
        line(' ', text)
    }
    for text in &a[prefix..a.len() - suffix] {
        line('-', text)
    }
    for text in &b[prefix..b.len() - suffix] {
        line('+', text)
    }
    for text in &a[a.len() - suffix..a.len() - suffix + tail] {
        line(' ', text)
    }
    out
}
fn read(e: &Engine, r: Query) -> Result<Value> {
    crate::pipeline::relative(&r.path)?;
    ensure!(
        !r.path.chars().any(char::is_control),
        invalid("control characters in diff path are unsupported")
    );
    let current = e.db.run(&r.run_id)?;
    let baseline = e.db.run(&r.baseline_id)?;
    ensure!(
        current.project_id == r.project_id && baseline.project_id == r.project_id,
        Fault::NotFound
    );
    let before = captured(e, &baseline, &r.path)?;
    let after = captured(e, &current, &r.path)?;
    ensure!(before.is_some() || after.is_some(), Fault::NotFound);
    Ok(
        json!({"project_id":r.project_id,"run_id":r.run_id,"baseline_id":r.baseline_id,"path":r.path,"before_exists":before.is_some(),"after_exists":after.is_some(),"diff":unified(&r.path,before.as_deref(),after.as_deref()),"verified":true,"algorithm":"bounded contiguous hunk with three context lines; not a minimal edit script"}),
    )
}
pub async fn compare(e: Arc<Engine>, r: Query) -> Result<Value> {
    tokio::task::spawn_blocking(move || read(&e, r)).await?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unified_handles_add_delete_and_missing_newline() {
        assert_eq!(unified("file", Some("same\n"), Some("same\n")), "");
        assert!(unified("file", None, Some("new\n")).contains("@@ -0,0 +1,1 @@\n+new\n"));
        assert!(unified("file", Some("old\n"), None).contains("@@ -1,1 +0,0 @@\n-old\n"));
        assert!(
            unified("file", Some("old"), Some("new")).contains(
                "-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n"
            )
        );
    }
}
