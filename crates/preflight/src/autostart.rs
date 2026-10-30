//! Per-user login policy only: never bootstrap, bootout, restart or stop a server.
use crate::{engine::Engine, model::invalid};
use anyhow::{Context, Result, ensure};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub admin_token: String,
    /// Omit to inspect. Changing startup requires the current settings revision.
    pub enabled: Option<bool>,
    pub expected_revision: Option<String>,
}
struct Agent {
    path: PathBuf,
    label: String,
    domain: String,
    body: String,
    manager: &'static str,
}
fn output(command: &str, args: &[&str]) -> Result<String> {
    let result = Command::new(command).args(args).output()?;
    ensure!(
        result.status.success(),
        "macOS startup command failed: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(String::from_utf8(result.stdout)?)
}
fn disabled(text: &str, label: &str) -> Result<bool> {
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once("=>")
            && key.trim().trim_matches('"') == label
        {
            return match value.trim().trim_end_matches(';') {
                "true" | "disabled" => Ok(true),
                "false" | "enabled" => Ok(false),
                _ => Err(invalid("Unknown launchd startup state")),
            };
        }
    }
    Ok(false)
}
fn plist(path: &Path, allow_link: bool) -> Result<Value> {
    let link = fs::symlink_metadata(path)?;
    ensure!(
        allow_link || !link.is_symlink(),
        "Startup definition must not be a symlink"
    );
    let meta = fs::metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= 65536,
        "Startup definition must be a regular plist below 64 KiB"
    );
    serde_json::from_str(&output(
        "/usr/bin/plutil",
        &[
            "-convert",
            "json",
            "-o",
            "-",
            path.to_str().context("startup path")?,
        ],
    )?)
    .map_err(Into::into)
}
fn validate_agent(value: &Value, label: &str, config: &Path) -> Result<()> {
    ensure!(
        value["Label"] == label,
        "Startup label belongs to another service"
    );
    ensure!(
        value["RunAtLoad"] == true,
        "Startup definition does not launch at login"
    );
    let args = value["ProgramArguments"]
        .as_array()
        .context("Startup arguments missing")?;
    ensure!(
        args.len() == 4 && args[1] == "--config" && args[3] == "serve",
        "Refusing to change an unrelated startup definition"
    );
    let saved =
        Path::new(args[2].as_str().context("Startup configuration path")?).canonicalize()?;
    ensure!(
        saved == config.canonicalize()?,
        "Startup definition uses another configuration"
    );
    ensure!(
        Path::new(args[0].as_str().context("Startup binary")?)
            .file_name()
            .is_some_and(|name| name == "preflight"),
        "Startup binary is not Preflight"
    );
    Ok(())
}
fn stable_binary() -> Result<PathBuf> {
    let binary = std::env::current_exe()?;
    if let Some(versions) = binary
        .parent()
        .and_then(Path::parent)
        .filter(|p| p.file_name().is_some_and(|n| n == "versions"))
    {
        let current = versions
            .parent()
            .context("installation root")?
            .join("current/preflight");
        if current.canonicalize().ok().as_ref() == Some(&binary) {
            return Ok(current);
        }
    }
    for parent in binary.ancestors() {
        if parent.file_name().is_some_and(|name| name == "Cellar") {
            let current = parent
                .parent()
                .context("Homebrew prefix")?
                .join("opt/preflight/bin/preflight");
            if current.canonicalize().ok().as_ref() == Some(&binary) {
                return Ok(current);
            }
        }
    }
    Ok(binary)
}
fn agent(e: &Engine) -> Result<Agent> {
    ensure!(
        cfg!(target_os = "macos"),
        "Login startup management is available on macOS only"
    );
    let config = e
        .config_file
        .get()
        .context("Start the server with a configuration file to manage login startup")?
        .canonicalize()?;
    let home = PathBuf::from(std::env::var_os("HOME").context("User home is unavailable")?);
    ensure!(home.is_absolute(), "User home must be an absolute path");
    let uid = output("/usr/bin/id", &["-u"])?;
    let uid = uid.trim();
    ensure!(
        !uid.is_empty() && uid != "0" && uid.bytes().all(|b| b.is_ascii_digit()),
        "Login startup requires a non-root macOS user"
    );
    let directory = home.join("Library/LaunchAgents");
    let (label, body) = crate::service::server_agent(&stable_binary()?, &config)?;
    for brew_label in ["sh.brew.preflight", "homebrew.mxcl.preflight"] {
        let path = directory.join(format!("{brew_label}.plist"));
        if path.exists() {
            let value = plist(&path, true)?;
            if validate_agent(&value, brew_label, &config).is_ok() {
                return Ok(Agent {
                    path,
                    label: brew_label.into(),
                    domain: format!("gui/{uid}"),
                    body,
                    manager: "homebrew",
                });
            }
        }
    }
    let path = directory.join(format!("{label}.plist"));
    if path.symlink_metadata().is_ok() {
        validate_agent(&plist(&path, false)?, &label, &config)?;
    }
    Ok(Agent {
        path,
        label,
        domain: format!("gui/{uid}"),
        body,
        manager: "preflight",
    })
}
fn state(agent: &Agent) -> Result<Value> {
    let overrides = output("/bin/launchctl", &["print-disabled", &agent.domain])?;
    Ok(
        json!({"supported":true,"available":true,"enabled":agent.path.exists()&&!disabled(&overrides,&agent.label)?,"label":agent.label,"path":agent.path,"manager":agent.manager,"scope":"user_login","current_server":"unchanged"}),
    )
}
pub fn status(e: &Engine) -> Value {
    match agent(e).and_then(|a| state(&a)) {
        Ok(value) => value,
        Err(error) => {
            json!({"supported":cfg!(target_os="macos"),"available":false,"reason":error.to_string()})
        }
    }
}
fn apply(agent: &Agent, enabled: bool) -> Result<()> {
    let created = enabled && !agent.path.exists();
    if created {
        fs::create_dir_all(agent.path.parent().context("LaunchAgents directory")?)?;
        let temporary = agent
            .path
            .with_file_name(format!(".preflight-startup-{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(agent.body.as_bytes())?;
            file.sync_all()?;
            fs::hard_link(&temporary, &agent.path)?;
            Ok(())
        })();
        let _ = fs::remove_file(temporary);
        result?;
    }
    if !agent.path.exists() {
        return Ok(());
    }
    let target = format!("{}/{}", agent.domain, agent.label);
    if let Err(error) = output(
        "/bin/launchctl",
        &[if enabled { "enable" } else { "disable" }, &target],
    ) {
        if created {
            let _ = fs::remove_file(&agent.path);
        }
        return Err(error);
    }
    Ok(())
}
pub fn request(e: &Engine, r: Request) -> Result<Value> {
    crate::settings::authorize(e, &r.admin_token)?;
    let Some(enabled) = r.enabled else {
        return Ok(status(e));
    };
    // Verify launchd access before preparing persistent service credentials.
    state(&agent(e)?)?;
    let path = e.config_file.get().context("Configuration file required")?;
    ensure!(
        r.expected_revision.as_deref() == Some(&crate::settings::digest(&fs::read(path)?)),
        invalid("Configuration changed; reopen settings before changing startup")
    );
    let mut expected = r.expected_revision;
    if enabled && e.current_config().admin_token_file.is_none() {
        let credential = e
            .current_config()
            .data_dir
            .join(format!(".preflight-admin-{}.token", uuid::Uuid::now_v7()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&credential)?;
        file.write_all(r.admin_token.as_bytes())?;
        file.sync_all()?;
        let mut config: crate::config::Config = toml::from_str(&fs::read_to_string(path)?)?;
        config.admin_token_file = Some(credential.clone());
        let result = crate::settings::request(
            e,
            crate::settings::Request {
                admin_token: r.admin_token.clone(),
                expected_revision: expected,
                configuration: Some(serde_json::to_value(config)?),
            },
        );
        match result {
            Ok(data) => expected = data["revision"].as_str().map(str::to_owned),
            Err(error) => {
                let _ = fs::remove_file(credential);
                return Err(error);
            }
        }
    }
    {
        let _guard = e
            .settings_gate
            .lock()
            .map_err(|_| invalid("Settings lock poisoned"))?;
        ensure!(
            expected.as_deref() == Some(&crate::settings::digest(&fs::read(path)?)),
            invalid("Configuration changed; reopen settings before changing startup")
        );
        apply(&agent(e)?, enabled)?;
    }
    crate::settings::request(
        e,
        crate::settings::Request {
            admin_token: r.admin_token,
            configuration: None,
            expected_revision: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launchd_state_is_exact_and_unknown_values_fail_closed() {
        let text = "disabled services = {\n \"local.preflight-a\" => true\n \"local.preflight-ab\" => false\n}";
        assert!(disabled(text, "local.preflight-a").unwrap());
        assert!(!disabled(text, "local.preflight-ab").unwrap());
        assert!(!disabled(text, "absent").unwrap());
        assert!(disabled("\"a\" => unknown", "a").is_err());
        assert!(disabled("\"a\" => disabled", "a").unwrap());
        assert!(!disabled("\"a\" => enabled", "a").unwrap());
    }
    #[test]
    fn rejects_another_configuration_and_extra_launch_commands() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        fs::write(&config, "config").unwrap();
        let valid = json!({"Label":"local.preflight-a","RunAtLoad":true,"ProgramArguments":["/tmp/preflight","--config",config,"serve"]});
        validate_agent(&valid, "local.preflight-a", &config).unwrap();
        let other = temp.path().join("other.toml");
        fs::write(&other, "config").unwrap();
        assert!(validate_agent(&valid, "local.preflight-a", &other).is_err());
        assert!(validate_agent(&valid, "different", &config).is_err());
        let other = temp.path().join("other.toml");
        fs::write(&other, "config").unwrap();
        assert!(validate_agent(&valid, "local.preflight-a", &other).is_err());
        let mut wrong = valid.clone();
        wrong["ProgramArguments"][3] = json!("run-due");
        assert!(validate_agent(&wrong, "local.preflight-a", &config).is_err());
        let mut wrong = valid;
        wrong["ProgramArguments"][0] = json!("/bin/sh");
        assert!(validate_agent(&wrong, "local.preflight-a", &config).is_err());
    }
}
