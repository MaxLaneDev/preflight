//! Admin-only configuration. Opt-in token required; changes apply live without cancelling runs.
use crate::{config::Config, engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
#[derive(Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Administrative credential; never persist in client settings or logs.
    pub admin_token: String,
    pub expected_revision: Option<String>,
    /// Omit to read. Saving requires a matching expected_revision.
    pub configuration: Option<Value>,
}
pub(crate) fn file_credential(path: &std::path::Path) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.is_symlink(),
        "admin_token_file must be a regular file, not a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "admin_token_file must have owner-only permissions (0600)"
        );
    }
    ensure!(
        (32..=514).contains(&metadata.len()),
        "admin_token_file must contain 32..512 ASCII token characters"
    );
    let mut file = std::fs::File::open(path)?;
    let opened = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.dev() == opened.dev() && metadata.ino() == opened.ino(),
            "admin_token_file changed during open"
        );
    }
    ensure!(opened.is_file(), "admin_token_file must be a regular file");
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(&mut file, 515), &mut contents)?;
    let token = contents.trim_end_matches(['\r', '\n']);
    ensure!(
        (32..=512).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic()),
        "admin_token_file must contain 32..512 ASCII token characters"
    );
    Ok(token.to_owned())
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    crate::hashing::hex(Sha256::digest(bytes))
}
fn authenticate(supplied: &str, expected: Option<&str>) -> Result<()> {
    let expected=expected.filter(|v|v.len()>=32).ok_or_else(|| invalid("Administrative settings disabled: configure PREFLIGHT_ADMIN_TOKEN or a private admin_token_file with at least 32 characters"))?;
    let a = Sha256::digest(supplied.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    ensure!(
        a.iter()
            .zip(b.iter())
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            == 0,
        invalid("Invalid administrative credential")
    );
    Ok(())
}
pub(crate) fn authorize(e: &Engine, supplied: &str) -> Result<()> {
    let configured = match std::env::var("PREFLIGHT_ADMIN_TOKEN") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => e
            .current_config()
            .admin_token_file
            .as_deref()
            .map(file_credential)
            .transpose()?,
        Err(_) => return Err(invalid("PREFLIGHT_ADMIN_TOKEN must be valid text")),
    };
    if let Some(value) = &configured {
        e.db.remember_secrets(vec![value.clone()]);
    }
    authenticate(supplied, configured.as_deref())
}
pub fn request(e: &Engine, r: Request) -> Result<Value> {
    authorize(e, &r.admin_token)?;
    update(e, r)
}
fn update(e: &Engine, r: Request) -> Result<Value> {
    let _guard = e
        .settings_gate
        .lock()
        .map_err(|_| anyhow::anyhow!("settings lock poisoned"))?;
    let path = e
        .config_file
        .get()
        .ok_or_else(|| invalid("No configuration file registered"))?;
    let original = std::fs::read(path)?;
    if let Some(value) = r.configuration {
        ensure!(
            r.expected_revision.as_deref() == Some(&digest(&original)),
            invalid("Configuration changed; reload settings before saving")
        );
        let config: Config =
            serde_json::from_value(value).map_err(|err| invalid(err.to_string()))?;
        let text = toml::to_string_pretty(&config)?;
        ensure!(text.len() <= 65536, invalid("configuration exceeds 64 KiB"));
        let temporary =
            path.with_file_name(format!(".preflight-settings-{}.toml", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.set_permissions(std::fs::metadata(path)?.permissions())?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            let mut checked = Config::load(&temporary).map_err(|err| invalid(err.to_string()))?;
            for executable in [
                &mut checked.codex_command,
                &mut checked.ssh_command,
                &mut checked.worker_launcher,
                &mut checked.docker_command,
            ]
            .into_iter()
            .flatten()
            {
                ensure!(
                    executable.is_absolute() && executable.is_file(),
                    invalid("command paths must be absolute existing files")
                );
                *executable = executable.canonicalize()?;
            }
            checked.data_dir = checked.data_dir.canonicalize().unwrap_or(checked.data_dir);
            crate::auth::validate_environment(&checked.access_tokens)?;
            let mut live = e.live_config.write().expect("configuration lock");
            ensure!(
                checked.version == live.version
                    && checked.bind == live.bind
                    && checked.worker_bind == live.worker_bind
                    && checked.data_dir == live.data_dir,
                invalid(
                    "version, bind, worker_bind, data_dir are startup-only; hot reload cannot replace listeners or the active database"
                )
            );
            ensure!(
                digest(&std::fs::read(path)?) == digest(&original),
                invalid("Configuration changed during validation")
            );
            std::fs::rename(&temporary, path)?;
            e.db.remember_secrets(crate::secrets::configured(&checked));
            e.scheduler
                .reconfigure(checked.max_parallel_commands, checked.memory_budget_mib);
            *live = checked;
            #[cfg(unix)]
            std::fs::File::open(path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
    }
    let bytes = std::fs::read(path)?;
    let saved: Config = toml::from_str(std::str::from_utf8(&bytes)?)?;
    let mut resolved = Config::load(path)?;
    // Engine resolves executable symlinks and the data directory during startup.
    resolved.data_dir = resolved
        .data_dir
        .canonicalize()
        .unwrap_or(resolved.data_dir);
    for command in [
        &mut resolved.codex_command,
        &mut resolved.ssh_command,
        &mut resolved.worker_launcher,
    ]
    .into_iter()
    .flatten()
    {
        *command = command.canonicalize().unwrap_or_else(|_| command.clone());
    }
    Ok(
        json!({"release_notifications_supported":true,"autostart":crate::autostart::status(e),"admin_token_file_supported":true,"ci_sources_supported":true,"disk_admission":true,"storage_size_admission":true,"storage_policy_quotas":true,"storage_policy_disk_pressure":true,"storage_policy_categories":crate::storage_policy::categories(),"path":path,"revision":digest(&bytes),"active":e.current_config(),"saved":saved,"restart_required":false,"reload_required":serde_json::to_value(resolved)?!=serde_json::to_value(e.current_config())?,"reload":{"mode":"hot","immutable_fields":["version","bind","worker_bind","data_dir"],"running_runs":"keep their admission configuration and resource requests","lower_limits":"held permits finish; oversized requests admitted before reload run exclusively"}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_file_credentials_are_bounded_and_not_serialized() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("admin-token");
        let token = "a".repeat(64);
        std::fs::write(&path, format!("{token}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(file_credential(&path).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(file_credential(&path).unwrap(), token);
        let config_path = temp.path().join("preflight.toml");
        std::fs::write(&config_path, "version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=[]\nadmin_token_file='admin-token'\n").unwrap();
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.admin_token_file.as_deref(),
            Some(path.canonicalize().unwrap().as_path())
        );
        assert!(!serde_json::to_string(&config).unwrap().contains(&token));
        assert!(crate::secrets::configured(&config).contains(&token));
        let mut filter = crate::secrets::Filter::new(crate::secrets::configured(&config));
        assert_eq!(filter.push(token.as_bytes(), true), b"[REDACTED]");
        authenticate(&token, Some(&file_credential(&path).unwrap())).unwrap();
        assert!(authenticate("wrong", Some(&file_credential(&path).unwrap())).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, temp.path().join("link")).unwrap();
            assert!(file_credential(&temp.path().join("link")).is_err());
        }
        for value in [
            "short".to_owned(),
            "b".repeat(514),
            format!("{} secret", "b".repeat(32)),
        ] {
            std::fs::write(&path, value).unwrap();
            assert!(file_credential(&path).is_err());
        }
    }
    #[test]
    fn administrative_access_is_opt_in() {
        assert!(authenticate("anything", None).is_err());
        assert!(authenticate("short", Some("short")).is_err());
        assert!(authenticate("wrong", Some(&"a".repeat(32))).is_err());
        assert!(authenticate(&"a".repeat(32), Some(&"a".repeat(32))).is_ok());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    #[test]
    fn saves_validated_config_and_applies_live_without_reopening_engine() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("preflight.toml");
        std::fs::write(
            &path,
            "version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=['.']\n",
        )
        .unwrap();
        let e = Engine::open(Config::load(&path).unwrap()).unwrap();
        e.config_file.set(path.clone()).unwrap();
        let read = || {
            update(
                &e,
                Request {
                    admin_token: String::new(),
                    expected_revision: None,
                    configuration: None,
                },
            )
            .unwrap()
        };
        let before = read();
        assert_eq!(before["restart_required"], false);
        let mut config = before["saved"].clone();
        config["max_parallel_commands"] = json!(5);
        config["max_active_runs"] = json!(3);
        config["memory_budget_mib"] = json!(512);
        config["default_stage_memory_mib"] = json!(128);
        config["storage_policies"] = json!([{
            "name":"weekly-workspaces",
            "project_id":uuid::Uuid::now_v7().to_string(),
            "pipeline_id":null,
            "category":"stage_workspaces",
            "ttl_seconds":604800,
            "interval_seconds":3600,
            "batch_limit":25
        }]);
        assert_eq!(
            before["storage_policy_categories"],
            json!([
                "stage_workspaces",
                "artifact_expiry",
                "logs",
                "source_snapshots",
                "module_copies",
                "cache_artifacts",
                "run_archives"
            ])
        );
        config["storage_policies"][0]["disk_pressure"] =
            json!({"start_below_bytes":1073741824,"stop_at_bytes":2147483648u64});
        let mut artifacts = config["storage_policies"][0].clone();
        artifacts["name"] = json!("weekly-artifacts");
        artifacts["category"] = json!("artifact_expiry");
        artifacts["quota"] = json!({"high_bytes":1073741824,"low_bytes":536870912});
        artifacts.as_object_mut().unwrap().remove("disk_pressure");
        config["storage_policies"]
            .as_array_mut()
            .unwrap()
            .push(artifacts);
        let saved = update(
            &e,
            Request {
                admin_token: String::new(),
                expected_revision: before["revision"].as_str().map(str::to_owned),
                configuration: Some(config.clone()),
            },
        )
        .unwrap();
        assert_eq!(saved["restart_required"], false);
        assert_eq!(saved["reload_required"], false);
        assert_eq!(e.current_config().max_parallel_commands, 5);
        assert_eq!(e.current_config().max_active_runs, 3);
        assert_eq!(e.scheduler_status()["capacity_slots"], 5);
        assert_eq!(e.current_config().storage_policies.len(), 2);
        assert_eq!(e.config.max_parallel_commands, 2);
        assert_eq!(e.config.max_active_runs, 32);
        assert_eq!(Config::load(&path).unwrap().max_active_runs, 3);
        assert_eq!(Config::load(&path).unwrap().max_parallel_commands, 5);
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.memory_budget_mib, Some(512));
        assert_eq!(loaded.default_stage_memory_mib, 128);
        assert_eq!(loaded.storage_policies.len(), 2);
        assert_eq!(
            loaded.storage_policies[1].quota.as_ref().unwrap().low_bytes,
            536870912
        );
        assert_eq!(loaded.storage_policies[0].ttl_seconds, 604800);
        assert_eq!(
            loaded.storage_policies[0]
                .disk_pressure
                .as_ref()
                .unwrap()
                .stop_at_bytes,
            2147483648
        );
        assert!(e.config.storage_policies.is_empty());
        assert!(e.config.memory_budget_mib.is_none());
        assert!(
            update(
                &e,
                Request {
                    admin_token: String::new(),
                    expected_revision: before["revision"].as_str().map(str::to_owned),
                    configuration: Some(config.clone())
                }
            )
            .is_err()
        );
        config["bind"] = json!("0.0.0.0:4170");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            update(
                &e,
                Request {
                    admin_token: String::new(),
                    expected_revision: saved["revision"].as_str().map(str::to_owned),
                    configuration: Some(config)
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn hot_reload_during_a_pipeline_keeps_snapshot_and_applies_limits_to_new_runs() {
        use crate::{
            model::{CommandSpec, RegisterProject, RunState, StartBuild},
            pipeline::StartPipeline,
        };
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let path = temp.path().join("preflight.toml");
        std::fs::write(&path, "version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=['a']\nmax_parallel_commands=2\nmemory_budget_mib=512\n").unwrap();
        let e = Engine::open(Config::load(&path).unwrap()).unwrap();
        e.config_file.set(path.clone()).unwrap();
        let p = e
            .register(RegisterProject {
                name: "a".into(),
                path: a.to_string_lossy().into(),
            })
            .unwrap();
        let started = temp.path().join("started");
        let release = temp.path().join("release");
        let run = e.start_pipeline(StartPipeline{project_id:p.id.clone(),definition:Some(serde_json::from_value(json!({"version":1,"pipeline":{"max_parallel_stages":1,"stages":[
            {"id":"hold","kind":"test","resources":{"slots":2,"memory_mib":512},"command":{"program":"sh","args":["-c","touch \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.02; done","preflight",started,release],"timeout_seconds":10}},
            {"id":"after","kind":"test","needs":["hold"],"resources":{"slots":2,"memory_mib":512},"command":{"program":"sh","args":["-c","echo still-running"],"timeout_seconds":10}}
        ]}})).unwrap())}).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let before = update(
            &e,
            Request {
                admin_token: String::new(),
                configuration: None,
                expected_revision: None,
            },
        )
        .unwrap();
        let mut configuration = before["saved"].clone();
        configuration["max_active_runs"] = json!(1);
        configuration["max_parallel_commands"] = json!(1);
        configuration["memory_budget_mib"] = json!(128);
        configuration["default_stage_memory_mib"] = json!(64);
        configuration["allowed_roots"] = json!(["b"]);
        let saved = update(
            &e,
            Request {
                admin_token: String::new(),
                expected_revision: before["revision"].as_str().map(str::to_owned),
                configuration: Some(configuration.clone()),
            },
        )
        .unwrap();
        assert_eq!(saved["reload_required"], false);
        assert_eq!(e.scheduler_status()["reserved_slots"], 2);
        assert_eq!(e.scheduler_status()["draining"], true);
        let pb = e
            .register(RegisterProject {
                name: "b".into(),
                path: b.to_string_lossy().into(),
            })
            .unwrap();
        let request = || StartBuild {
            project_id: pb.id.clone(),
            command: CommandSpec {
                program: "sh".into(),
                args: vec!["-c".into(), "echo new-settings".into()],
                timeout_seconds: 10,
            },
        };
        assert!(e.start(request()).is_err());
        std::fs::write(release, "go").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !e.active.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(e.db.run(&run.id).unwrap().status, RunState::Succeeded);
        let next = e.start(request()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !e.active.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(e.db.run(&next.id).unwrap().status, RunState::Succeeded);
        let latest = update(
            &e,
            Request {
                admin_token: String::new(),
                configuration: None,
                expected_revision: None,
            },
        )
        .unwrap();
        let mut disk = latest["saved"].clone();
        disk["max_storage_mib"] = json!(1);
        let payload = std::fs::File::create(e.config.data_dir.join("quota-test.bin")).unwrap();
        payload.set_len(2 * 1024 * 1024).unwrap();
        update(
            &e,
            Request {
                admin_token: String::new(),
                configuration: Some(disk),
                expected_revision: Some(latest["revision"].as_str().unwrap().into()),
            },
        )
        .unwrap();
        let denied = e.start(request()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !e.db.run(&denied.id).unwrap().status.terminal() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            e.db.run(&denied.id).unwrap().status,
            RunState::InfrastructureError
        );
        assert_eq!(e.db.run(&run.id).unwrap().status, RunState::Succeeded);

        assert_eq!(e.scheduler_status()["available_memory_mib"], 128);
        let bytes = std::fs::read(&path).unwrap();
        configuration["bind"] = json!("127.0.0.1:4179");
        assert!(
            update(
                &e,
                Request {
                    admin_token: String::new(),
                    expected_revision: saved["revision"].as_str().map(str::to_owned),
                    configuration: Some(configuration)
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        e.shutdown().await.unwrap();
    }
}
