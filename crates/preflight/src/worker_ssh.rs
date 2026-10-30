//! Foreground SSH provisioning; the same worker executable runs behind a reverse tunnel.
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub job_id: String,
    pub process_stopped: bool,
}
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::worker_runtime::Policy>,
    pub project_id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "ssh_port")]
    pub port: u16,
    #[serde(default = "remote_port")]
    pub remote_port: u16,
    pub bundle_dir: PathBuf,
    #[serde(default)]
    pub remote_dir: Option<PathBuf>,
    #[serde(default)]
    pub once: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
}
fn ssh_port() -> u16 {
    22
}
fn remote_port() -> u16 {
    4171
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub(crate) fn validate(r: &Request) -> Result<()> {
    uuid::Uuid::parse_str(&r.project_id)?;
    if let Some(runtime) = &r.runtime {
        runtime.validate()?;
    }
    crate::control::validate_name(&r.name)?;
    ensure!(
        !r.host.is_empty()
            && r.host.len() <= 255
            && !r.host.starts_with('-')
            && r.host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-.@:[]".contains(&c)),
        "invalid SSH host; use a hostname or configured SSH alias"
    );
    ensure!(
        r.port > 0 && r.remote_port > 0,
        "SSH and tunnel ports must be nonzero"
    );
    if let Some(recovery) = &r.recovery {
        uuid::Uuid::parse_str(&recovery.job_id)
            .map_err(|_| crate::model::invalid("invalid recovery job ID"))?;
        ensure!(
            recovery.process_stopped && !r.once,
            "journal recovery requires process_stopped=true and once=false"
        );
    }
    if let Some(path) = &r.remote_dir {
        let value = path.to_str().context("remote path must be UTF-8")?;
        ensure!(
            path.is_absolute() && value.len() <= 4096 && !value.chars().any(char::is_control),
            "invalid remote installation directory"
        );
    }
    Ok(())
}
fn ssh(r: &Request, config: &crate::config::Config) -> tokio::process::Command {
    let mut command =
        tokio::process::Command::new(config.ssh_command.as_deref().unwrap_or(Path::new("ssh")));
    command.args([
        "-T",
        "-a",
        "-x",
        "-oBatchMode=yes",
        "-oStrictHostKeyChecking=yes",
        "-oConnectTimeout=10",
        "-oServerAliveInterval=5",
        "-oServerAliveCountMax=2",
        "-oExitOnForwardFailure=yes",
        "-p",
        &r.port.to_string(),
    ]);
    command.kill_on_drop(true);
    command
}
async fn output(mut command: tokio::process::Command, input: Option<&Path>) -> Result<Vec<u8>> {
    command.stdin(match input {
        Some(path) => std::process::Stdio::from(std::fs::File::open(path)?),
        None => std::process::Stdio::null(),
    });
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("start SSH (OpenSSH client is required)")?;
    let mut stdout = child.stdout.take().context("SSH stdout")?.take(8193);
    let mut stderr = child.stderr.take().context("SSH stderr")?.take(8193);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let (a, b, status) = tokio::join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        );
        a?;
        b?;
        let status = status?;
        ensure!(
            out.len() <= 8192 && err.len() <= 8192,
            "SSH reply exceeds limit"
        );
        ensure!(
            status.success(),
            "SSH step failed: {}",
            String::from_utf8_lossy(&err)
        );
        Ok::<_, anyhow::Error>(())
    })
    .await;
    match result {
        Ok(result) => result?,
        Err(_) => {
            child.kill().await.ok();
            anyhow::bail!("SSH step timed out; inspect remote installation before retrying");
        }
    }
    Ok(out)
}
fn platform(bytes: &[u8]) -> Result<(&str, &str)> {
    let text = std::str::from_utf8(bytes)?;
    let mut lines = text.lines();
    let os = match lines.next() {
        Some("Linux") => "linux",
        Some("Darwin") => "macos",
        _ => anyhow::bail!("SSH bootstrap supports Linux and macOS"),
    };
    let arch = match lines.next() {
        Some("x86_64" | "amd64") => "x86_64",
        Some("aarch64" | "arm64") => "aarch64",
        _ => anyhow::bail!("unsupported remote architecture"),
    };
    ensure!(lines.next().is_none(), "unexpected platform probe response");
    Ok((os, arch))
}
fn install_script(r: &Request, manifest: &crate::worker_package::Manifest) -> Result<String> {
    // All dynamic values are shell-quoted; credentials never enter this command.
    let namespace = format!("{}/{}", r.project_id, r.name);
    let root = match &r.remote_dir {
        Some(path) => quote(path.to_str().context("remote path encoding")?),
        None => format!(
            "\"$HOME/.local/share/preflight/workers/\"{}",
            quote(&namespace)
        ),
    };
    Ok(format!(
        r#"set -eu
umask 077
pf_bundle=$(mktemp -d)
trap 'rm -rf "$pf_bundle"' EXIT HUP INT TERM
pf_root={root}
printf '%s' {manifest} > "$pf_bundle/manifest.json"
cat > "$pf_bundle/preflight"
if command -v sha256sum >/dev/null 2>&1; then pf_hash=$(sha256sum "$pf_bundle/preflight");
elif command -v shasum >/dev/null 2>&1; then pf_hash=$(shasum -a 256 "$pf_bundle/preflight");
else echo 'sha256sum or shasum is required' >&2; exit 1; fi
[ "${{pf_hash%% *}}" = {digest} ] || {{ echo 'transferred worker SHA-256 mismatch' >&2; exit 1; }}
chmod 700 "$pf_bundle/preflight"
"$pf_bundle/preflight" worker install "$pf_bundle" "$pf_root"
"#,
        root = root,
        manifest = quote(&serde_json::to_string(manifest)?),
        digest = quote(&manifest.sha256)
    ))
}
pub async fn run(config: &crate::config::Config, r: Request) -> Result<()> {
    run_mode(config, r, false).await
}
pub async fn resume(config: &crate::config::Config, r: Request) -> Result<()> {
    run_mode(config, r, true).await
}
async fn run_mode(config: &crate::config::Config, r: Request, resume_only: bool) -> Result<()> {
    validate(&r)?;
    let resume_only = resume_only || r.recovery.is_some();
    ensure!(!resume_only || !r.once, "resume requires once=false");
    let gateway = config
        .worker_bind
        .context("server worker_bind must be configured and running before SSH provisioning")?;
    ensure!(
        gateway.ip().is_ipv4(),
        "SSH launcher currently requires an IPv4 loopback worker_bind"
    );
    let mut probe = ssh(&r, config);
    probe.args(["-oClearAllForwardings=yes", &r.host, "uname -s; uname -m"]);
    let detected = output(probe, None).await?;
    let (os, arch) = platform(&detected)?;
    let bundle = r.bundle_dir.join(format!("{os}-{arch}"));
    let (manifest, binary) = crate::worker_package::verify(&bundle, os, arch)
        .with_context(|| format!("verified worker package required at {}", bundle.display()))?;
    ensure!(
        manifest.cli_protocol == crate::worker_package::CLI_PROTOCOL,
        "managed SSH requires worker package v2 with session lifetime support; repackage with this server release"
    );
    let mut install = ssh(&r, config);
    install.args([
        "-oClearAllForwardings=yes",
        &r.host,
        &install_script(&r, &manifest)?,
    ]);
    let installed: Value = serde_json::from_slice(&output(install, Some(&binary)).await?)?;
    let executable = installed["binary"]
        .as_str()
        .context("installer did not return executable path")?;
    let state = installed["state"]
        .as_str()
        .context("installer did not return state path")?;
    for value in [executable, state] {
        ensure!(
            Path::new(value).is_absolute()
                && value.len() <= 4096
                && !value.chars().any(char::is_control),
            "invalid remote installation path"
        );
    }
    ensure!(
        r.runtime.is_none() || manifest.job_protocol >= crate::worker_jobs::CONTAINER_PROTOCOL,
        "worker runtime policy requires a protocol 4 worker package"
    );
    let token = if resume_only {
        None
    } else {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        let enrollment: Value = client
        .post(format!("http://{}/api/v1/worker-enrollments", config.bind))
        .json(
            &json!({"project_id":r.project_id,"name":r.name,"os":os,"arch":arch,"ttl_seconds":60}),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
        let token = enrollment["enrollment_token"]
            .as_str()
            .context("missing enrollment token")?;
        ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid enrollment token"
        );
        Some(token.to_owned())
    };
    let mut connection = ssh(&r, config);
    let forward = format!("127.0.0.1:{}:127.0.0.1:{}", r.remote_port, gateway.port());
    let mut remote = format!(
        "exec {} worker {} http://127.0.0.1:{}/ {}{} --ssh-session",
        quote(executable),
        if resume_only { "resume" } else { "connect" },
        r.remote_port,
        quote(state),
        if r.once { " --once" } else { "" }
    );
    if let Some(recovery) = &r.recovery {
        remote = format!(
            "{} worker recover http://127.0.0.1:{}/ {} {} --process-stopped 1>&2 && {remote}",
            quote(executable),
            r.remote_port,
            quote(state),
            quote(&recovery.job_id)
        );
    }
    if let Some(runtime) = &r.runtime {
        remote = format!(
            "{} worker runtime {} {} 1>&2 && {remote}",
            quote(executable),
            quote(state),
            quote(&serde_json::to_string(runtime)?)
        );
    }
    connection
        .args(["-R", &forward, &r.host, &remote])
        .stdin(std::process::Stdio::piped());
    let mut child = connection.spawn().context("start worker SSH tunnel")?;
    let mut control = child.stdin.take().context("SSH session stdin")?;
    if let Some(token) = token {
        control.write_all(format!("{token}\n").as_bytes()).await?;
    }
    eprintln!(
        "Worker installed for {os}/{arch}; waiting for authenticated connection. Keep this command running to keep the SSH tunnel open."
    );
    let status = tokio::select! { result=child.wait()=>result?, result=tokio::signal::ctrl_c()=>{result?;child.kill().await?;return Ok(());} };
    drop(control);
    ensure!(
        status.success(),
        "SSH worker connection ended unsuccessfully; inspect host-key trust, tunnel port, enrollment and worker journal"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_and_quoting_are_strict() {
        assert_eq!(platform(b"Linux\nx86_64\n").unwrap(), ("linux", "x86_64"));
        assert_eq!(platform(b"Darwin\narm64\n").unwrap(), ("macos", "aarch64"));
        assert!(platform(b"Linux\nx86_64\nextra\n").is_err());
        assert_eq!(quote("a'b"), "'a'\\''b'");
        let mut r = Request {
            runtime: None,
            project_id: uuid::Uuid::now_v7().to_string(),
            name: "worker".into(),
            host: "user@laptop".into(),
            port: 22,
            remote_port: 4171,
            bundle_dir: "bundle".into(),
            remote_dir: None,
            once: false,
            recovery: None,
        };
        assert!(validate(&r).is_ok());
        for host in ["-oProxyCommand=bad", "host;echo x", "host\ncommand", "a b"] {
            r.host = host.into();
            assert!(validate(&r).is_err());
        }
    }
}
