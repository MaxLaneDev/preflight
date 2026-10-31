use anyhow::{Context, Result, bail, ensure};
use preflight::{
    config::Config,
    engine::Engine,
    model::*,
    pipeline::{PipelineFile, StageKind, StartPipeline},
};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|v| v == "package") {
        println!("{}", preflight::distribution::run(&args[1..]).await?);
        return Ok(());
    }
    if args == ["codex-hook"] {
        preflight::distribution::hook()?;
        return Ok(());
    }
    if args == ["--version"] {
        println!(
            "{}",
            serde_json::json!({"component":"preflight","version":env!("CARGO_PKG_VERSION"),"protocol":preflight::control::PROTOCOL_VERSION,"schema":preflight::migrations::VERSION,"os":std::env::consts::OS,"arch":std::env::consts::ARCH})
        );
        return Ok(());
    }
    if args == ["credential", "generate"] {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("credential entropy unavailable"))?;
        println!(
            "{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        return Ok(());
    }
    if args.is_empty() || args[0] == "--help" {
        println!(
            "preflight --version\npreflight credential generate\npreflight package create --server BINARY --destination NEW_DIR --repository SOURCE\npreflight package verify ARCHIVE --sha256 SHA256\npreflight package install ARCHIVE --sha256 SHA256 --prefix BINARIES --config CONFIG [--data DATA]\npreflight package status|rollback|uninstall --prefix BINARIES\npreflight codex-hook\npreflight backup create CONFIG NEW_ARCHIVE\npreflight backup verify ARCHIVE SHA256\npreflight backup restore ARCHIVE SHA256 NEW_DIRECTORY\npreflight backup activation-preview RESTORED_DIR CONFIG\npreflight backup activate RESTORED_DIR CONFIG PLAN_ID\npreflight archive keygen NEW_IDENTITY_FILE\npreflight archive seal INPUT NEW_OUTPUT RECIPIENT\npreflight archive unseal INPUT NEW_OUTPUT SHA256 IDENTITY_FILE\npreflight [--config FILE] serve\npreflight [--config FILE] run-due\npreflight [--config FILE] service export launchd|systemd|cron|windows NEW_DIRECTORY\npreflight [--config FILE] build PATH [-- PROGRAM ARG...]\npreflight [--config FILE] test PATH\npreflight [--config FILE] fixtures run PATH\npreflight [--config FILE] status RUN_ID\npreflight [--config FILE] cancel RUN_ID [REASON]\npreflight worker connect LOOPBACK_GATEWAY PRIVATE_STATE_DIR [--once|--job-once]\npreflight worker resume LOOPBACK_GATEWAY PRIVATE_STATE_DIR\npreflight worker journal PRIVATE_STATE_DIR\npreflight worker recover LOOPBACK_GATEWAY PRIVATE_STATE_DIR JOB_ID --process-stopped\npreflight worker service export launchd|systemd LOOPBACK_GATEWAY PRIVATE_STATE_DIR NEW_DIRECTORY\npreflight worker fetch PRIVATE_STATE_DIR TRANSFER_ID TREE_SHA256\npreflight [--config FILE] worker-ssh REQUEST.json\npreflight worker package DESTINATION\npreflight worker bootstrap BUNDLE GATEWAY INSTALL_DIR [--once]\nBuild submits to a running local server and returns immediately."
        );
        return Ok(());
    }
    if args[0] == "backup" {
        let report = match args.get(1).map(String::as_str) {
            Some("create") if args.len() == 4 => {
                preflight::server_backup::create(Path::new(&args[2]), Path::new(&args[3]))?
            }
            Some("verify") if args.len() == 4 => {
                preflight::server_backup::verify(Path::new(&args[2]), &args[3])?
            }
            Some("restore") if args.len() == 5 => preflight::server_backup::restore(
                Path::new(&args[2]),
                &args[3],
                Path::new(&args[4]),
            )?,
            Some("activation-preview") if args.len() == 4 => {
                preflight::server_backup::activation_preview(
                    Path::new(&args[2]),
                    Path::new(&args[3]),
                )?
            }
            Some("activate") if args.len() == 5 => preflight::server_backup::activate(
                Path::new(&args[2]),
                Path::new(&args[3]),
                &args[4],
            )?,
            _ => bail!(
                "usage: preflight backup create CONFIG NEW_ARCHIVE | verify ARCHIVE SHA256 | restore ARCHIVE SHA256 NEW_DIRECTORY | activation-preview RESTORED_DIR CONFIG | activate RESTORED_DIR CONFIG PLAN_ID"
            ),
        };
        println!("{report}");
        return Ok(());
    }
    if args[0] == "archive" {
        let report = match args.get(1).map(String::as_str) {
            Some("keygen") if args.len() == 3 => {
                preflight::archive_crypto::keygen(Path::new(&args[2]))?
            }
            Some("seal") if args.len() == 5 => {
                preflight::archive_crypto::seal(Path::new(&args[2]), Path::new(&args[3]), &args[4])?
            }
            Some("unseal") if args.len() == 6 => preflight::archive_crypto::unseal(
                Path::new(&args[2]),
                Path::new(&args[3]),
                &args[4],
                Path::new(&args[5]),
            )?,
            _ => bail!(
                "usage: preflight archive keygen NEW_IDENTITY_FILE | seal INPUT NEW_OUTPUT RECIPIENT | unseal INPUT NEW_OUTPUT SHA256 IDENTITY_FILE"
            ),
        };
        println!("{report}");
        return Ok(());
    }
    if args[0] == "worker" {
        if args.get(1).is_some_and(|v| v == "runtime") {
            ensure!(
                args.len() == 4,
                "usage: preflight worker runtime PRIVATE_STATE_DIR POLICY_JSON"
            );
            preflight::worker_runtime::save(Path::new(&args[2]), serde_json::from_str(&args[3])?)?;
            println!("{}", serde_json::json!({"runtime_configured":true}));
            return Ok(());
        }
        if args.get(1).is_some_and(|v| v == "containers-cleanup") {
            ensure!(
                args.len() == 4 || (args.len() == 5 && args[4] == "--creation-settled"),
                "usage: preflight worker containers-cleanup PRIVATE_STATE_DIR JOB_ID [--creation-settled]"
            );
            println!(
                "{}",
                preflight::worker_client::cleanup_container_journal(
                    Path::new(&args[2]),
                    &args[3],
                    args.len() == 5
                )
                .await?
            );
            return Ok(());
        }
        if args.get(1).is_some_and(|arg| arg == "journal") {
            ensure!(
                args.len() == 3,
                "usage: preflight worker journal PRIVATE_STATE_DIR"
            );
            println!(
                "{}",
                preflight::worker_client::journal_status(Path::new(&args[2]))?
            );
            return Ok(());
        }
        if args.get(1).is_some_and(|arg| arg == "recover") {
            ensure!(
                args.len() == 6 && args[5] == "--process-stopped",
                "usage: preflight worker recover LOOPBACK_GATEWAY PRIVATE_STATE_DIR JOB_ID --process-stopped"
            );
            println!(
                "{}",
                preflight::worker_client::recover(&args[2], Path::new(&args[3]), &args[4], true)
                    .await?
            );
            return Ok(());
        }
        if args.len() == 5 && args[1] == "resume" && args[4] == "--ssh-session" {
            return preflight::worker_client::ssh_session(
                &args[2],
                Path::new(&args[3]),
                false,
                true,
            )
            .await;
        }
        if args.len() == 4 && args[1] == "resume" {
            return preflight::worker_client::resume(&args[2], Path::new(&args[3])).await;
        }
        if args.get(1).is_some_and(|arg| arg == "service") {
            ensure!(
                args.len() == 7 && args[2] == "export",
                "usage: preflight worker service export launchd|systemd LOOPBACK_GATEWAY PRIVATE_STATE_DIR NEW_DIRECTORY"
            );
            println!(
                "{}",
                preflight::service::export_worker(
                    &args[3],
                    &std::env::current_exe()?,
                    &args[4],
                    Path::new(&args[5]),
                    Path::new(&args[6])
                )?
            );
            return Ok(());
        }
        if args.len() == 4 && args[1] == "install" {
            let root = std::path::PathBuf::from(&args[3]);
            let binary = preflight::worker_package::install(Path::new(&args[2]), &root)?;
            println!(
                "{}",
                serde_json::json!({"binary":binary,"state":root.join("state")})
            );
            return Ok(());
        }
        if args.len() == 3 && args[1] == "package" {
            println!(
                "{}",
                serde_json::to_string(&preflight::worker_package::package(Path::new(&args[2]))?)?
            );
            return Ok(());
        }
        if (args.len() == 5 || (args.len() == 6 && args[5] == "--once")) && args[1] == "bootstrap" {
            let root = std::path::PathBuf::from(&args[4]);
            let binary = preflight::worker_package::install(Path::new(&args[2]), &root)?;
            let mut command = std::process::Command::new(binary);
            command
                .args(["worker", "connect", &args[3]])
                .arg(root.join("state"));
            if args.len() == 6 {
                command.arg("--once");
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                return Err(command.exec().into());
            }
            #[cfg(not(unix))]
            {
                ensure!(
                    command.status()?.success(),
                    "installed worker connection failed"
                );
                return Ok(());
            }
        }

        if args.len() == 5 && args[1] == "fetch" {
            return preflight::worker_client::fetch(Path::new(&args[2]), &args[3], &args[4]).await;
        }
        if args.len() >= 5
            && args.len() <= 6
            && args[1] == "connect"
            && args.last().is_some_and(|a| a == "--ssh-session")
        {
            ensure!(
                args.len() == 5 || args[4] == "--once",
                "SSH connect supports only optional --once"
            );
            return preflight::worker_client::ssh_session(
                &args[2],
                Path::new(&args[3]),
                args.len() == 6,
                false,
            )
            .await;
        }
        ensure!(
            (args.len() == 4
                || (args.len() == 5 && matches!(args[4].as_str(), "--once" | "--job-once")))
                && args[1] == "connect",
            "usage: preflight worker connect LOOPBACK_GATEWAY PRIVATE_STATE_DIR [--once|--job-once]; enrollment token is read from stdin only on first connection"
        );
        return preflight::worker_client::connect(
            &args[2],
            Path::new(&args[3]),
            args.get(4).is_some_and(|a| a == "--once"),
            args.get(4).is_some_and(|a| a == "--job-once"),
        )
        .await;
    }
    let config_path = if args.first().map(String::as_str) == Some("--config") {
        ensure!(args.len() >= 3, "--config requires FILE and a subcommand");
        let path = args.remove(1);
        args.remove(0);
        path
    } else {
        "preflight.toml".to_owned()
    };
    let config = Config::load(Path::new(&config_path))?;
    if args[0] == "service" {
        ensure!(
            args.len() == 4 && args[1] == "export",
            "usage: preflight [--config FILE] service export launchd|systemd|cron|windows NEW_DIRECTORY"
        );
        println!(
            "{}",
            preflight::service::export(
                &args[2],
                &std::env::current_exe()?,
                Path::new(&config_path),
                Path::new(&args[3])
            )?
        );
        return Ok(());
    }

    if args[0] == "worker-ssh" {
        ensure!(
            args.len() == 2 || (args.len() == 3 && args[2] == "--resume"),
            "usage: preflight [--config FILE] worker-ssh REQUEST.json [--resume]"
        );
        let bytes = preflight::fixtures::read(Path::new(&args[1]), 16384)?;
        return if args.len() == 3 {
            preflight::worker_ssh::resume(&config, serde_json::from_slice(&bytes)?).await
        } else {
            preflight::worker_ssh::run(&config, serde_json::from_slice(&bytes)?).await
        };
    }
    if args[0] == "serve" {
        ensure!(args.len() == 1, "usage: preflight [--config FILE] serve");
        let engine = Engine::open(config.clone())?;
        engine
            .config_file
            .set(Path::new(&config_path).canonicalize()?)
            .map_err(|_| anyhow::anyhow!("config file already set"))?;
        let listener = tokio::net::TcpListener::bind(config.bind).await?;
        eprintln!("Preflight listening at http://{}", listener.local_addr()?);
        let gateway_stop = tokio_util::sync::CancellationToken::new();
        let gateway = if let Some(bind) = config.worker_bind {
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!(
                "Preflight worker gateway at http://{}",
                listener.local_addr()?
            );
            let app = preflight::workers::router(engine.clone());
            let stop = gateway_stop.clone();
            Some(tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stop.cancelled_owned())
                    .await
            }))
        } else {
            None
        };
        let releases = tokio::spawn(preflight::releases::run(engine.clone()));
        let bridge = tokio::spawn(preflight::codex_bridge::run(engine.clone()));
        let queues = tokio::spawn(preflight::queues::run(engine.clone()));
        let worker_jobs = tokio::spawn(preflight::worker_jobs::reaper(engine.clone()));
        let worker_connections = tokio::spawn(preflight::worker_connections::run(engine.clone()));
        let schedules = tokio::spawn(preflight::schedules::run(engine.clone()));
        let storage_policy = tokio::spawn(preflight::storage_policy::run(engine.clone()));
        let log_archive = tokio::spawn(preflight::log_archive::run(engine.clone()));
        let containers = tokio::spawn(preflight::docker::run(engine.clone()));
        let ci_import = tokio::spawn(preflight::ci::run(engine.clone()));
        let diagnostic_index = tokio::spawn(preflight::diagnostics::run_indexer(engine.clone()));
        let shutdown_engine = engine.clone();
        let shutdown_gateway = gateway_stop.clone();
        axum::serve(listener, preflight::api::router(engine.clone()))
            .with_graceful_shutdown(async move {
                if let Err(error) = shutdown_signal().await {
                    eprintln!("shutdown signal: {error:#}");
                }
                shutdown_gateway.cancel();
                if let Err(error) = shutdown_engine.shutdown().await {
                    eprintln!("shutdown: {error:#}");
                }
            })
            .await?;
        engine.shutdown().await?;
        gateway_stop.cancel();
        if let Some(gateway) = gateway {
            gateway.await??;
        }
        releases.await?;
        schedules.await?;
        log_archive.await?;
        diagnostic_index.await?;
        ci_import.await?;
        containers.await?;
        storage_policy.await?;
        queues.await?;
        worker_jobs.await?;
        worker_connections.await?;
        bridge.await?;
        return Ok(());
    }
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(token) = std::env::var("PREFLIGHT_API_TOKEN") {
        ensure!(
            (32..=512).contains(&token.len()) && token.bytes().all(|c| c.is_ascii_graphic()),
            "PREFLIGHT_API_TOKEN must contain 32..512 non-whitespace ASCII characters"
        );
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let base = format!("http://{}/api/v1", config.bind);
    if args[0] == "fixtures" {
        ensure!(
            args.len() == 3 && args[1] == "run",
            "usage: preflight fixtures run PATH"
        );
        args.remove(1);
    }
    let response = match args[0].as_str() {
        "build" | "test" | "fixtures" => {
            ensure!(
                args.len() == 2 || (args[0] == "build" && args.len() >= 4 && args[2] == "--"),
                "usage: preflight build PATH [-- PROGRAM ARG...] or preflight test PATH"
            );
            let root = Path::new(&args[1]).canonicalize()?;
            let name = root
                .file_name()
                .context("project name")?
                .to_string_lossy()
                .into_owned();
            let project: Project = decode(
                client
                    .post(format!("{base}/projects"))
                    .json(&RegisterProject {
                        name,
                        path: root.to_string_lossy().into_owned(),
                    })
                    .send()
                    .await?,
            )
            .await?;
            if args[0] == "fixtures" {
                client
                    .post(format!("{base}/batches"))
                    .json(&serde_json::json!({"project_id":project.id}))
                    .send()
                    .await?
            } else if args.len() == 2 {
                if args[0] == "test" {
                    let file: PipelineFile = decode(
                        client
                            .get(format!("{base}/projects/{}/pipeline", project.id))
                            .send()
                            .await?,
                    )
                    .await?;
                    ensure!(
                        file.pipeline
                            .stages
                            .iter()
                            .any(|s| s.kind == StageKind::Test),
                        "configured pipeline has no test stages"
                    );
                    client
                        .post(format!("{base}/pipeline-runs"))
                        .json(&StartPipeline {
                            project_id: project.id,
                            definition: Some(file),
                        })
                        .send()
                        .await?
                } else {
                    client
                        .post(format!("{base}/pipeline-runs"))
                        .json(&StartPipeline {
                            project_id: project.id,
                            definition: None,
                        })
                        .send()
                        .await?
                }
            } else {
                client
                    .post(format!("{base}/runs"))
                    .json(&StartBuild {
                        project_id: project.id,
                        command: CommandSpec {
                            program: args[3].clone(),
                            args: args[4..].to_vec(),
                            timeout_seconds: 300,
                        },
                    })
                    .send()
                    .await?
            }
        }
        "status" => {
            ensure!(args.len() == 2, "usage: preflight status RUN_ID");
            let id = valid_id(&args[1])?;
            client
                .get(format!("{base}/runs/{id}/report"))
                .send()
                .await?
        }
        "run-due" => {
            ensure!(args.len() == 1, "usage: preflight [--config FILE] run-due");
            client
                .post(format!("{base}/schedules/run-due"))
                .json(&serde_json::json!({}))
                .send()
                .await?
        }
        "cancel" => {
            ensure!(
                (2..=3).contains(&args.len()),
                "usage: preflight cancel RUN_ID [REASON]"
            );
            let id = valid_id(&args[1])?;
            client.post(format!("{base}/runs/{id}/cancel")).json(&serde_json::json!({"actor":"user","reason":args.get(2).map(String::as_str).unwrap_or("Cancelled from CLI")})).send().await?
        }
        _ => bail!("unknown command; use --help"),
    };
    let value: serde_json::Value = decode(response).await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn valid_id(value: &str) -> Result<uuid::Uuid> {
    Ok(value.parse()?)
}
async fn decode<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        bail!("server returned {status}: {}", response.text().await?)
    }
    Ok(response.json().await?)
}
async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result?,_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
