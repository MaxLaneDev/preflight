//! Native remote stages share preparation and collection with the local engine.
use crate::{
    engine::Engine,
    executor::Execution,
    model::{CommandSpec, RunState},
    pipeline::{ArtifactKind, StageDefinition},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
const ROOT: &str = "${preflight.workspace}/";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}
pub fn validate(selector: &Selector) -> Result<()> {
    for value in [&selector.os, &selector.arch] {
        ensure!(
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "invalid worker OS/architecture"
        );
    }
    if let Some(id) = &selector.worker_id {
        uuid::Uuid::parse_str(id)?;
    }
    Ok(())
}
fn map_values(
    command: &mut CommandSpec,
    env: &mut BTreeMap<String, String>,
    replace: impl Fn(&str) -> Result<String>,
) -> Result<()> {
    command.program = replace(&command.program)?;
    for value in command.args.iter_mut().chain(env.values_mut()) {
        *value = replace(value)?;
    }
    Ok(())
}
/// Only whole prepared-path values are relocated; no command-line string parsing.
fn relocate(
    workspace: &Path,
    command: &mut CommandSpec,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    map_values(command, env, |value| {
        if let Ok(relative) = Path::new(value).strip_prefix(workspace) {
            let path = relative
                .to_str()
                .context("non-UTF8 stage input path")?
                .replace('\\', "/");
            ensure!(!path.is_empty(), "stage root reference must name an input");
            return Ok(format!("{ROOT}{path}"));
        }
        ensure!(
            !value.contains("${preflight.workspace}"),
            "reserved worker workspace marker"
        );
        Ok(value.into())
    })
}
pub(crate) fn expand(
    workspace: &Path,
    command: &mut CommandSpec,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    map_values(command, env, |value| {
        if let Some(relative) = value.strip_prefix(ROOT) {
            ensure!(
                relative.split('/').all(crate::trees::portable_component),
                "invalid prepared input path"
            );
            let mut path = workspace.to_owned();
            for component in relative.split('/') {
                path.push(component);
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => {
                        ensure!(!metadata.is_symlink(), "symlink in prepared input path")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            return Ok(path.to_str().context("non-UTF8 worker workspace")?.into());
        }
        ensure!(
            !value.contains("${preflight.workspace}"),
            "invalid worker workspace reference"
        );
        Ok(value.into())
    })
}
fn returned_paths(stage: &StageDefinition) -> Result<BTreeMap<String, ArtifactKind>> {
    let mut paths = BTreeMap::new();
    let path = |value: &str| -> Result<String> {
        Ok(crate::pipeline::relative(&stage.cwd)?
            .join(crate::pipeline::relative(value)?)
            .to_str()
            .context("non-UTF8 returned path")?
            .replace('\\', "/"))
    };
    for output in stage.outputs.values() {
        paths.insert(path(&output.path)?, output.kind);
    }
    for report in stage.report_paths() {
        let report = path(report)?;
        if !paths.iter().any(|(root, kind)| {
            *kind == ArtifactKind::Directory && report.starts_with(&format!("{root}/"))
        }) {
            paths.entry(report).or_insert(ArtifactKind::File);
        }
    }
    crate::worker_outputs::validate_paths(&paths)?;
    Ok(paths)
}

pub async fn execute(
    engine: Arc<Engine>,
    selector: &Selector,
    stage: &StageDefinition,
    input: Execution<'_>,
    cancel: &CancellationToken,
) -> Result<(RunState, Option<i32>)> {
    if cancel.is_cancelled() {
        return Ok((RunState::Cancelled, None));
    }
    let run = engine.db.run(input.run_id)?;
    let selected: Option<(String,u64)> = engine.db.connection()?.query_row(
        "SELECT w.id,w.generation FROM workers w WHERE w.project_id=?1 AND w.os=?2 AND w.arch=?3 AND w.state='registered' AND w.job_protocol IN (3,4) AND (?6=0 OR w.job_protocol=4) AND w.last_seen>?4 AND (?5 IS NULL OR w.id=?5) AND NOT EXISTS(SELECT 1 FROM worker_jobs j JOIN operation_leases l ON l.id=j.operation_id WHERE j.worker_id=w.id AND l.state='abandoned') ORDER BY (SELECT count(*) FROM worker_jobs j WHERE j.worker_id=w.id AND j.state IN ('queued','leased','cancelling')),w.id LIMIT 1",
        params![run.project_id,selector.os,selector.arch,crate::workers::now()-30,selector.worker_id,stage.container.is_some()], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (worker_id, generation) = selected.context(
        "no online project worker matches the requested OS/architecture and recovery state",
    )?;
    if let Some(toolchain) = engine.db.toolchain(input.run_id)? {
        ensure!(
            toolchain.contract["host_os"] == selector.os
                && toolchain.contract["host_arch"] == selector.arch,
            "pinned toolchain host differs from selected worker; prepare a worker-compatible toolchain first"
        );
    }
    let returns = returned_paths(stage)?;
    let mut command = input.command.clone();
    let mut env = input.env.clone();
    relocate(input.workspace, &mut command, &mut env)?;
    let transfer = crate::worker_transfers::prepare_stage(
        engine.clone(),
        crate::worker_transfers::Prepare {
            run_id: input.run_id.into(),
            worker_id,
            generation,
            ttl_seconds: 600,
        },
        input.workspace.to_owned(),
    )
    .await?;
    if cancel.is_cancelled() {
        return Ok((RunState::Cancelled, None));
    }
    let config = engine.current_config();
    let requested = config.stage_memory(stage)?;
    let memory_mib = if requested == 0 {
        config.default_stage_memory_mib
    } else {
        requested
    };
    let job = engine.db.worker_job_enqueue(
        input.run_id,
        input.stage_id,
        transfer["transfer_id"]
            .as_str()
            .context("missing transfer ID")?,
        &crate::worker_jobs::Spec {
            container: stage
                .container
                .clone()
                .map(|spec| crate::worker_jobs::Container { spec, memory_mib }),
            command,
            cwd: stage.cwd.clone(),
            env,
            returns: returns.clone(),
        },
    )?;
    let id = job["job_id"].as_str().context("missing job ID")?;
    let (state, result) = loop {
        engine.db.worker_jobs_reap()?;
        let row: (String, Option<String>) = engine.db.connection()?.query_row(
            "SELECT state,result_json FROM worker_jobs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !matches!(row.0.as_str(), "queued" | "leased" | "cancelling") {
            break row;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    ensure!(
        state != "lost",
        "worker job {id} lost its lease or registration; execution will not be retried"
    );
    let status: RunState = serde_json::from_value(state.into())?;
    let Some(result) = result else {
        return Ok((status, None));
    };
    let outcome: crate::worker_jobs::Outcome = serde_json::from_str(&result)?;
    let accepted_cursor: i64 = engine.db.connection()?.query_row(
        "SELECT log_cursor FROM worker_jobs WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let streamed = outcome.log_cursor.is_some() || accepted_cursor > 0;
    for (stream, text) in [("stdout", &outcome.stdout), ("stderr", &outcome.stderr)] {
        if streamed && !(stream == "stderr" && status == RunState::InfrastructureError) {
            continue;
        }
        if !text.is_empty() {
            engine
                .db
                .append_stage_log(input.run_id, input.stage_id, stream, 1, text)?;
        }
    }
    if outcome.truncated && outcome.log_cursor.is_none() {
        engine.db.append_stage_log(
            input.run_id,
            input.stage_id,
            "stderr",
            2,
            "[worker output truncated]\n",
        )?;
    }
    if matches!(status, RunState::Succeeded | RunState::Failed)
        && !returns.is_empty()
        && !cancel.is_cancelled()
    {
        let (directory, digest): (String, String) = engine.db.connection()?.query_row(
            "SELECT directory,tree_sha256 FROM worker_outputs WHERE job_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let stored =
            crate::pipeline::contained(&engine.config.data_dir, "worker-outputs")?.join(directory);
        let destination = input.workspace.to_owned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let parent = destination.parent().context("stage parent")?;
            let temporary = tempfile::tempdir_in(parent)?;
            let copied = temporary.path().join("tree");
            let manifest = crate::trees::copy(&stored, &copied)?;
            ensure!(
                manifest.sha256()? == digest,
                "stored worker result digest changed before collection"
            );
            for (path, kind) in returns {
                let source = crate::pipeline::contained(&copied, &path)?;
                if source.try_exists()? {
                    let target = crate::pipeline::contained(&destination, &path)?;
                    ensure!(
                        !target.try_exists()?,
                        "worker output would overwrite a stage input"
                    );
                    std::fs::create_dir_all(target.parent().context("output parent")?)?;
                    crate::artifacts::copy_artifact(&source, &target, kind)?;
                }
            }
            Ok(())
        })
        .await??;
    }
    Ok((status, outcome.exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_paths_relocate_without_server_absolute_paths_or_traversal() {
        let root = tempfile::tempdir().unwrap();
        let server = root.path().join("server");
        let worker = root.path().join("worker");
        std::fs::create_dir_all(worker.join(".preflight/inputs/build")).unwrap();
        let path = server
            .join(".preflight/inputs/build/tool")
            .to_str()
            .unwrap()
            .to_owned();
        let mut command = CommandSpec {
            program: path.clone(),
            args: vec![path.clone(), "literal argument".into()],
            timeout_seconds: 5,
        };
        let mut env = BTreeMap::from([("SDK".into(), path)]);
        relocate(&server, &mut command, &mut env).unwrap();
        assert!(command.program.starts_with(ROOT));
        expand(&worker, &mut command, &mut env).unwrap();
        assert_eq!(
            Path::new(&command.program),
            worker.join(".preflight/inputs/build/tool")
        );
        assert_eq!(command.args[0], command.program);
        assert_eq!(env["SDK"], command.program);
        assert_eq!(command.args[1], "literal argument");
        for suffix in ["../outside", "a/../../outside", "/absolute", "a\\b"] {
            command.program = format!("{ROOT}{suffix}");
            assert!(expand(&worker, &mut command, &mut env).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&server, worker.join("link")).unwrap();
            command.program = format!("{ROOT}link/tool");
            assert!(expand(&worker, &mut command, &mut env).is_err());
        }
    }
    #[test]
    fn report_inside_declared_directory_uses_one_return_root() {
        let mut stage = StageDefinition::native(CommandSpec {
            program: "tool".into(),
            args: vec![],
            timeout_seconds: 5,
        });
        stage.cwd = "suite".into();
        stage.outputs.insert(
            "reports".into(),
            crate::pipeline::OutputSpec {
                path: "reports".into(),
                kind: ArtifactKind::Directory,
            },
        );
        stage.junit.push("reports/tests.xml".into());
        assert_eq!(
            returned_paths(&stage).unwrap(),
            BTreeMap::from([("suite/reports".into(), ArtifactKind::Directory)])
        );
        stage.worker = Some(Selector {
            os: "linux".into(),
            arch: "x86_64".into(),
            worker_id: None,
        });
        assert!(validate(stage.worker.as_ref().unwrap()).is_ok());
    }
}
