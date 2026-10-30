//! Compact generic run comparison; experiment and run paths share the same domain comparison.
use crate::{engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::{Value, json};
fn limit() -> usize {
    20
}
fn budget() -> usize {
    8192
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub run_id: String,
    pub baseline_id: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default = "budget")]
    pub max_bytes: usize,
}
pub fn compare(e: &Engine, r: &Query) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit) && (4096..=65536).contains(&r.max_bytes),
        invalid("limit must be 1..100; max_bytes 4096..65536")
    );
    let mut raw = crate::experiments::compare_runs(
        e,
        &crate::experiments::Compare {
            project_id: r.project_id.clone(),
            run_id: r.run_id.clone(),
            baseline_id: Some(r.baseline_id.clone()),
            offset: r.offset,
        },
    )?;
    if let Some(diff) = raw
        .get_mut("inputs")
        .and_then(|v| v.get_mut("diff"))
        .and_then(Value::as_object_mut)
    {
        diff.remove("changes");
        diff.remove("next_cursor");
    }
    raw["input_details_tool"] = json!("runs_compare_inputs");
    raw["diagnostics"] = json!({"tool":"diagnostics_report","project_id":r.project_id,"run_id":r.run_id,"baseline_id":r.baseline_id});
    let names = ["test_changes", "artifact_changes", "stage_changes"];
    let total = [
        "test_changes_total",
        "artifact_changes_total",
        "stage_changes_total",
    ]
    .iter()
    .filter_map(|key| raw[*key].as_u64())
    .max()
    .unwrap_or(0) as usize;
    let mut count = r.limit;
    loop {
        let mut result = raw.clone();
        for name in names {
            if let Some(rows) = result[name].as_array_mut() {
                rows.truncate(count);
                for row in rows {
                    if let Some(object) = row.as_object_mut() {
                        let mut truncated = false;
                        for value in object.values_mut() {
                            if let Some(text) = value.as_str() {
                                let shortened = text.chars().take(256).collect::<String>();
                                if shortened.len() < text.len() {
                                    *value = json!(shortened);
                                    truncated = true;
                                }
                            }
                        }
                        if truncated {
                            object.insert("sample_truncated".into(), json!(true));
                        }
                    }
                }
            }
        }
        result["next_cursor"] = if r.offset + count < total {
            json!(r.offset + count)
        } else {
            Value::Null
        };
        if serde_json::to_vec(&result)?.len() <= r.max_bytes {
            return Ok(result);
        }
        ensure!(
            count > 1,
            invalid("comparison metadata exceeds response budget; increase max_bytes")
        );
        count -= 1;
    }
}
