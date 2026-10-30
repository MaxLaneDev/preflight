//! Bounded global metadata and redacted-log retrieval; no definitions or raw secrets in results.
use crate::{catalog, control, engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{functions::FunctionFlags, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub query: String,
    pub project_id: Option<String>,
    pub after: Option<String>,
    #[serde(default = "page_size")]
    pub limit: usize,
}
fn page_size() -> usize {
    20
}
pub(crate) fn matches_query(text: &str, query: &str) -> bool {
    text.to_lowercase().contains(&query.trim().to_lowercase())
}
fn rank(needle: &str, title: &str, id: &str, query: &str) -> i64 {
    let id = id.to_lowercase();
    if title.to_lowercase() == query || id == query {
        0
    } else if needle.to_lowercase().contains(query) || id.contains(query) {
        1
    } else {
        -1
    }
}

#[allow(clippy::too_many_arguments)]
fn offer(
    rows: &mut BTreeMap<(i64, String), Value>,
    after: &(i64, String),
    rank: i64,
    kind: &str,
    id: &str,
    project: &str,
    title: &str,
    detail: &str,
    target: Value,
    limit: usize,
) {
    let key = format!("{kind}:{project}:{id}");
    if rank < 0 || (rank, key.clone()) <= *after {
        return;
    }
    rows.insert((rank,key.clone()),json!({"key":key,"kind":kind,"id":id,"project_id":project,"title":title,"detail":detail,"target":target}));
    while rows.len() > limit + 1 {
        rows.pop_last();
    }
}
pub fn search(e: &Engine, r: &Query) -> Result<Value> {
    let query = r.query.trim();
    ensure!(
        !query.is_empty()
            && query.len() <= 256
            && !query.chars().any(char::is_control)
            && (1..=100).contains(&r.limit),
        invalid("query must be 1..256 bytes without control characters; limit must be 1..100")
    );
    if let Some(id) = &r.project_id {
        e.db.project(id)?;
    }
    let after: (i64, String) = match &r.after {
        Some(s) => {
            ensure!(s.len() <= 8192, invalid("invalid search cursor"));
            serde_json::from_str(s).map_err(|_| invalid("invalid search cursor"))?
        }
        None => (-1, String::new()),
    };
    ensure!(
        (-1..=1).contains(&after.0),
        invalid("invalid search cursor")
    );
    let q = query
        .strip_prefix('#')
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(query)
        .to_lowercase();
    let mut rows = BTreeMap::new();
    let projects = e.db.projects()?;
    let projects = projects
        .into_iter()
        .filter(|p| r.project_id.as_ref().is_none_or(|id| id == &p.id))
        .collect::<Vec<_>>();
    let c = e.db.connection()?;
    c.create_scalar_function(
        "preflight_search_rank",
        4,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
        |ctx| {
            Ok(rank(
                &ctx.get::<String>(0)?,
                &ctx.get::<String>(1)?,
                &ctx.get::<String>(2)?,
                &ctx.get::<String>(3)?,
            ))
        },
    )?;
    let sql = r#"WITH visible AS (
 SELECT r.id run_id,r.project_id,p.pipeline_key,p.name,r.status FROM runs r JOIN run_pipeline_identity p ON p.run_id=r.id
 WHERE (?2 IS NULL OR r.project_id=?2) AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.restored_at IS NULL AND ((t.kind='run' AND t.target_id=r.id) OR(t.kind='pipeline' AND t.target_id=p.pipeline_key)))
), objects(kind,id,project_id,run_id,stage_id,test_name,title,detail,needle) AS (
 SELECT 'project',id,id,NULL,NULL,NULL,name,root,id||' '||name FROM projects WHERE ?2 IS NULL OR id=?2
 UNION ALL SELECT 'pipeline',pipeline_key,project_id,MAX(run_id),NULL,NULL,name,'Pipeline',pipeline_key||' '||name FROM visible WHERE NOT EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=visible.run_id) GROUP BY project_id,pipeline_key
 UNION ALL SELECT 'run',run_id,project_id,run_id,NULL,NULL,name||' · '||substr(run_id,-8),status,run_id||' '||name||' '||COALESCE((SELECT c.provider_run_id||' '||COALESCE(c.git_sha,'')||' '||c.provider||' '||COALESCE(json_extract(c.metadata_json,'$.number'),'') FROM ci_runs c WHERE c.run_id=visible.run_id),'') FROM visible
 UNION ALL SELECT 'stage',s.id,v.project_id,v.run_id,s.name,NULL,s.name,s.status,s.id||' '||s.name||' '||COALESCE((SELECT json_extract(j.metadata_json,'$.name') FROM ci_jobs j WHERE j.run_id=s.run_id AND j.stage_id=s.name),'') FROM stage_runs s JOIN visible v ON v.run_id=s.run_id
 UNION ALL SELECT 'test',v.run_id||':'||f.stage_id||':'||f.case_id,v.project_id,v.run_id,f.stage_id,f.case_id,f.case_id,f.status,f.case_id||' '||CAST(f.ordinal+1 AS TEXT) FROM fixture_results f JOIN visible v ON v.run_id=f.run_id
 UNION ALL SELECT 'test',v.run_id||':'||s.name||':'||json_extract(j.value,'$.id'),v.project_id,v.run_id,s.name,json_extract(j.value,'$.name'),json_extract(j.value,'$.name'),json_extract(j.value,'$.status'),coalesce(json_extract(j.value,'$.id'),'')||' '||coalesce(json_extract(j.value,'$.name'),'')||' '||coalesce(json_extract(j.value,'$.class_name'),'')||' '||coalesce(json_extract(j.value,'$.suite'),'') FROM stage_runs s JOIN visible v ON v.run_id=s.run_id,json_each(s.tests_json,'$.cases') j
 UNION ALL SELECT 'artifact',v.run_id||':'||a.stage_id||':'||a.name,v.project_id,v.run_id,a.stage_id,NULL,a.name,a.sha256,a.name||' '||a.sha256 FROM artifacts a JOIN visible v ON v.run_id=a.run_id
 UNION ALL SELECT 'module',m.sha256,m.project_id,v.run_id,json_extract(m.artifact_json,'$.stage_id'),NULL,'Module '||substr(m.sha256,1,12),m.sha256,m.sha256 FROM modules m JOIN visible v ON v.run_id=json_extract(m.artifact_json,'$.run_id')
 UNION ALL SELECT 'input',v.run_id||':'||j.key,v.project_id,v.run_id,NULL,NULL,j.key,json_extract(j.value,'$.sha256'),j.key||' '||coalesce(json_extract(j.value,'$.sha256'),'') FROM run_inputs i JOIN visible v ON v.run_id=i.run_id,json_each(i.manifest_json,'$.entries') j WHERE json_extract(j.value,'$.kind')='file'
 UNION ALL SELECT 'log',CAST(l.id AS TEXT),v.project_id,v.run_id,l.stage_id,NULL,'Log #'||l.id,substr(f.text,1,240),CAST(l.id AS TEXT) FROM log_events l JOIN visible v ON v.run_id=l.run_id JOIN log_search f ON f.rowid=l.id WHERE l.id=CAST(?1 AS INTEGER)
 UNION ALL SELECT 'source',v.run_id||':'||j.fullkey,v.project_id,v.run_id,NULL,NULL,'Source '||j.key,j.value,j.value FROM run_sources src JOIN visible v ON v.run_id=src.run_id,json_tree(src.request_json) j WHERE j.type='text' AND j.key IN ('commit','base_commit','sha256','tree_sha256','snapshot_sha256')
 UNION ALL SELECT 'stagekey',v.run_id||':'||k.stage_id,v.project_id,v.run_id,k.stage_id,NULL,k.stage_id||' key',json_extract(k.evidence_json,'$.sha256'),json_extract(k.evidence_json,'$.sha256') FROM stage_keys k JOIN visible v ON v.run_id=k.run_id
 UNION ALL SELECT 'cache',c.id,v.project_id,v.run_id,c.producer_stage,NULL,'Stage cache',c.cache_key,c.cache_key FROM stage_cache c JOIN visible v ON v.run_id=c.producer_run
), ranked AS (SELECT *,kind||':'||project_id||':'||id search_key,preflight_search_rank(coalesce(needle,''),coalesce(title,''),id,?1) rank FROM objects)
SELECT kind,id,project_id,run_id,stage_id,test_name,title,coalesce(detail,''),rank FROM ranked WHERE rank>=0 AND (rank>?3 OR(rank=?3 AND search_key>?4)) ORDER BY rank,search_key LIMIT ?5"#;
    let mut statement = c.prepare(sql)?;
    let found = statement
        .query_map(
            params![q, r.project_id, after.0, after.1, r.limit + 1],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, id, project, run, stage, test, title, detail, rank) in found {
        let mut target = json!({"project":project,"section":"pipelines"});
        if let Some(run) = run {
            let (pipeline,experiment):(String,Option<String>)=c.query_row("SELECT p.pipeline_key,x.name FROM run_pipeline_identity p LEFT JOIN experiment_runs x ON x.run_id=p.run_id WHERE p.run_id=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?)))?;
            target["pipeline"] = experiment
                .as_ref()
                .map_or(pipeline, |s| format!("experiment:{s}"))
                .into();
            if experiment.is_some() {
                target["section"] = "experiments".into();
            }
            target["run"] = run.into();
            target["tab"] = match kind.as_str() {
                "test" => "Tests",
                "artifact" | "module" => "Artifacts",
                "input" | "cache" | "stagekey" => "Paths",
                "source" => "Changes",
                "log" => "Logs",
                _ => "Overview",
            }
            .into();
            if let Some(stage) = stage {
                target["stage"] = stage.into();
            }
            if let Some(test) = test {
                target["test"] = test.into();
            }
        }
        offer(
            &mut rows, &after, rank, &kind, &id, &project, &title, &detail, target, r.limit,
        );
    }
    // Logs use only the durable redacted FTS index, including compressed events.
    let terms = q
        .split_whitespace()
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    ensure!(terms.len() <= 16, invalid("at most 16 search terms"));
    let mut logs=c.prepare("SELECT l.id,r.project_id,l.run_id,l.stage_id,p.pipeline_key,snippet(log_search,0,'','',' … ',20),x.name FROM log_search JOIN log_events l ON l.id=log_search.rowid JOIN runs r ON r.id=l.run_id JOIN run_pipeline_identity p ON p.run_id=r.id LEFT JOIN experiment_runs x ON x.run_id=r.id WHERE log_search MATCH ?1 AND CAST(l.id AS TEXT)!=?6 AND (?2 IS NULL OR r.project_id=?2) AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.restored_at IS NULL AND ((t.kind='run' AND t.target_id=r.id)OR(t.kind='pipeline' AND t.target_id=p.pipeline_key))) AND (1>?3 OR (1=?3 AND 'log:'||r.project_id||':'||l.id>?4)) ORDER BY 'log:'||r.project_id||':'||l.id LIMIT ?5")?;
    for row in logs.query_map(
        params![
            terms.join(" AND "),
            r.project_id,
            after.0,
            after.1,
            r.limit + 1,
            q
        ],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        },
    )? {
        let (id, project, run, stage, pipeline, excerpt, experiment) = row?;
        let target = json!({"project":project,"section":if experiment.is_some(){"experiments"}else{"pipelines"},"pipeline":experiment.map_or(pipeline,|s|format!("experiment:{s}")),"run":run,"tab":"Logs","stage":stage});
        offer(
            &mut rows,
            &after,
            1,
            "log",
            &id.to_string(),
            &project,
            &format!("Log #{id}"),
            &excerpt.chars().take(240).collect::<String>(),
            target,
            r.limit,
        );
    }
    drop(logs);
    drop(statement);
    drop(c);
    let mut unavailable = Vec::new();
    for project in &projects {
        for kind in ["saved_pipelines", "experiments"] {
            let mut cursor = None;
            loop {
                let page = match catalog::list(
                    e,
                    &catalog::Query {
                        kind: kind.into(),
                        project_id: Some(project.id.clone()),
                        query: String::new(),
                        after: cursor,
                        limit: 100,
                    },
                ) {
                    Ok(v) => v,
                    Err(_) => {
                        unavailable.push(project.id.clone());
                        break;
                    }
                };
                for item in page["items"].as_array().into_iter().flatten() {
                    let name = item["name"].as_str().unwrap_or_default();
                    let section = if kind == "experiments" {
                        "experiments"
                    } else {
                        "pipelines"
                    };
                    let key = if kind == "experiments" {
                        format!("experiment:{name}")
                    } else {
                        format!("saved:{name}")
                    };
                    let revision = item["revision"].as_u64().unwrap_or(0);
                    let sha = control::config_path(
                        &project.root,
                        &format!(".preflight/{section}/{name}/{revision}.json"),
                    )
                    .and_then(|p| crate::fixtures::read(&p, 128 * 1024))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .and_then(|v| v["sha256"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                    let rank = rank(&format!("{name} {sha} {key}"), name, &key, &q);
                    offer(
                        &mut rows,
                        &after,
                        rank,
                        if kind == "experiments" {
                            "experiment"
                        } else {
                            "pipeline"
                        },
                        &key,
                        &project.id,
                        name,
                        &format!("Revision {revision}"),
                        json!({"project":project.id,"section":section,"pipeline":key,"tab":"Graph"}),
                        r.limit,
                    );
                }
                cursor = page["next_cursor"].as_str().map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
        }
    }
    unavailable.sort();
    unavailable.dedup();
    let more = rows.len() > r.limit;
    while rows.len() > r.limit {
        rows.pop_last();
    }
    let next = if more {
        rows.last_key_value()
            .map(|(key, _)| serde_json::to_string(key))
            .transpose()?
    } else {
        None
    };
    let mut results = rows.into_values().collect::<Vec<_>>();
    for hit in &mut results {
        if let Some(p) = projects
            .iter()
            .find(|p| Some(p.id.as_str()) == hit["project_id"].as_str())
        {
            hit["project_name"] = p.name.clone().into();
        }
    }
    Ok(
        json!({"query":r.query,"results":results,"next_cursor":next,"unavailable_projects":unavailable}),
    )
}
