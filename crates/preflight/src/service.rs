//! Export explicit per-user service definitions; exporting never activates a service.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

fn text(path: &Path) -> Result<&str> {
    let value = path.to_str().context("service paths must be UTF-8")?;
    ensure!(
        !value.chars().any(char::is_control),
        "service paths cannot contain control characters"
    );
    Ok(value)
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unit(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

fn windows_arg(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        out.push_str(&"\\".repeat(if ch == '\"' { slashes * 2 + 1 } else { slashes }));
        out.push(ch);
        slashes = 0;
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('\"');
    out
}

pub fn export(
    platform: &str,
    binary: &Path,
    config: &Path,
    destination: &Path,
) -> Result<serde_json::Value> {
    ensure!(
        matches!(platform, "launchd" | "systemd" | "cron" | "windows"),
        "supported service adapters: launchd, systemd, cron, windows"
    );
    let binary = binary.canonicalize()?;
    let config = config.canonicalize()?;
    crate::config::Config::load(&config)?;
    let binary = text(&binary)?;
    let config_text = text(&config)?;
    let cwd = text(config.parent().context("config directory")?)?;
    let digest = crate::hashing::hex(Sha256::digest(config_text.as_bytes()));
    let name = format!("preflight-{}", &digest[..12]);
    let files = render(platform, &name, binary, config_text, cwd)?;
    write_export(platform, &name, destination, files)
}

pub(crate) fn server_agent(binary: &Path, config: &Path) -> Result<(String, String)> {
    let config = config.canonicalize()?;
    let config_text = text(&config)?;
    let digest = crate::hashing::hex(Sha256::digest(config_text.as_bytes()));
    let name = format!("preflight-{}", &digest[..12]);
    let files = render(
        "launchd",
        &name,
        text(binary)?,
        config_text,
        text(config.parent().context("config directory")?)?,
    )?;
    Ok((
        format!("local.{name}"),
        files.into_iter().next().context("server agent")?.1,
    ))
}

fn write_export(
    platform: &str,
    name: &str,
    destination: &Path,
    files: Vec<(String, String)>,
) -> Result<serde_json::Value> {
    // A new directory prevents replacing an existing installation or operator edits.
    fs::create_dir(destination).context("service export requires a new destination directory")?;
    let mut names = Vec::new();
    for (name, body) in files {
        let path = destination.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        names.push(path);
    }
    Ok(serde_json::json!({"service":name,"platform":platform,"files":names,"activated":false}))
}

/// Export an already enrolled worker; credentials remain in its private state directory.
pub fn export_worker(
    platform: &str,
    binary: &Path,
    gateway: &str,
    state: &Path,
    destination: &Path,
) -> Result<serde_json::Value> {
    ensure!(
        matches!(platform, "launchd" | "systemd"),
        "supported worker service adapters: launchd, systemd"
    );
    crate::worker_client::validate_service_identity(gateway, state)?;
    let binary = binary.canonicalize()?;
    let state = text(state)?;
    let name = format!(
        "preflight-worker-{}",
        &crate::hashing::hex(Sha256::digest(state.as_bytes()))[..12]
    );
    let files = render_worker(platform, &name, text(&binary)?, gateway, state)?;
    write_export(platform, &name, destination, files)
}

fn render_worker(
    platform: &str,
    name: &str,
    binary: &str,
    gateway: &str,
    state: &str,
) -> Result<Vec<(String, String)>> {
    match platform {
        "launchd" => {
            let label = format!("local.{name}");
            Ok(vec![
                (
                    format!("{label}.plist"),
                    format!(
                        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{binary}</string><string>worker</string><string>connect</string><string>{gateway}</string><string>{state}</string></array>
<key>WorkingDirectory</key><string>{state}</string>
<key>StandardInPath</key><string>/dev/null</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>10</integer>
<key>ExitTimeOut</key><integer>300</integer>
</dict></plist>
"#,
                        binary = xml(binary),
                        gateway = xml(gateway),
                        state = xml(state)
                    ),
                ),
                (
                    "README.txt".into(),
                    format!(
                        "Per-user worker agent; export does not activate it. Stop the foreground worker first.\nCopy {label}.plist to ~/Library/LaunchAgents/, then run:\nlaunchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist\nStatus: launchctl print gui/$(id -u)/{label}\nStop: launchctl bootout gui/$(id -u)/{label}\nStarts at user login; not before login.\n{}",
                        WORKER_SERVICE_NOTES
                    ),
                ),
            ])
        }
        "systemd" => Ok(vec![
            (
                format!("{name}.service"),
                format!(
                    "[Unit]\nDescription=Preflight native worker\nStartLimitIntervalSec=0\n[Service]\nType=simple\nWorkingDirectory={}/\nExecStart=:{} worker connect {} {}\nStandardInput=null\nRestart=always\nRestartSec=10\nTimeoutStopSec=300\nUMask=0077\n[Install]\nWantedBy=default.target\n",
                    state.replace('%', "%%"),
                    unit(binary),
                    unit(gateway),
                    unit(state)
                ),
            ),
            (
                "README.txt".into(),
                format!(
                    "Per-user worker service; export does not activate it. Stop the foreground worker first.\nCopy {name}.service to ~/.config/systemd/user/, then run:\nsystemctl --user daemon-reload\nsystemctl --user enable --now {name}.service\nStatus: systemctl --user status {name}.service\nLogs: journalctl --user -u {name}.service\nStop: systemctl --user disable --now {name}.service\nStarts with the user manager. Before-login operation requires an operator-configured lingering user manager.\n{}",
                    WORKER_SERVICE_NOTES
                ),
            ),
        ]),
        _ => bail!("supported worker service adapters: launchd, systemd"),
    }
}
const WORKER_SERVICE_NOTES: &str = "The service reuses the enrolled private identity; no credential or enrollment token is embedded. Gateway/tunnel availability is external: this service does not install or maintain an SSH tunnel. Transport failures exit and are retried after 10 seconds. Revoked credentials need explicit re-enrollment. Existing state locks prevent a second worker from owning the same identity. Worker jobs and child processes receive normal cancellation on service stop; verify terminal server state before reclaiming artifacts. Use an absolute immutable installed binary path, and regenerate the export after upgrading it. Service environments do not inherit shell compiler/SDK paths; configure required environment explicitly.\n";

fn render(
    platform: &str,
    name: &str,
    binary: &str,
    config: &str,
    cwd: &str,
) -> Result<Vec<(String, String)>> {
    match platform {
        "launchd" => {
            let definition = |label: &str, command: &str, policy: &str| {
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{binary}</string><string>--config</string><string>{config}</string><string>{command}</string></array>
<key>WorkingDirectory</key><string>{cwd}</string>
{policy}
</dict></plist>
"#,
                    binary = xml(binary),
                    config = xml(config),
                    cwd = xml(cwd)
                )
            };
            let label = format!("local.{name}");
            Ok(vec![
                (
                    format!("{label}.plist"),
                    definition(
                        &label,
                        "serve",
                        "<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>",
                    ),
                ),
                (
                    format!("{label}.due.plist"),
                    definition(
                        &format!("{label}.due"),
                        "run-due",
                        "<key>StartInterval</key><integer>60</integer>",
                    ),
                ),
                (
                    "README.txt".into(),
                    format!(
                        "Per-user launchd definitions; export does not activate them.\nStop a manually started server using this config before loading the server agent.\nCopy {label}.plist to ~/Library/LaunchAgents/, then run:\nlaunchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist\nStatus: launchctl print gui/$(id -u)/{label}\nStop: launchctl bootout gui/$(id -u)/{label}\nThe optional {label}.due.plist invokes run-due every 60 seconds; load it the same way only if an OS trigger is desired. The server already has an internal scheduler.\nNo enrollment tokens or environment secrets are embedded. A server agent runs at user login, not before login.\n"
                    ),
                ),
            ])
        }
        "systemd" => {
            let command = format!(":{} --config {}", unit(binary), unit(config));
            let working_directory = format!("{}/", cwd.replace('%', "%%"));
            Ok(vec![
                (
                    format!("{name}.service"),
                    format!(
                        "[Unit]\nDescription=Preflight local server\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={command} serve\nRestart=on-failure\nRestartSec=10\nTimeoutStopSec=300\n[Install]\nWantedBy=default.target\n",
                        working_directory
                    ),
                ),
                (
                    format!("{name}-due.service"),
                    format!(
                        "[Unit]\nDescription=Dispatch due Preflight schedules\nAfter={name}.service\n[Service]\nType=oneshot\nWorkingDirectory={}\nExecStart={command} run-due\n",
                        working_directory
                    ),
                ),
                (
                    format!("{name}-due.timer"),
                    format!(
                        "[Unit]\nDescription=Check due Preflight schedules\n[Timer]\nOnBootSec=60\nOnUnitActiveSec=60\nUnit={name}-due.service\n[Install]\nWantedBy=timers.target\n"
                    ),
                ),
                (
                    "README.txt".into(),
                    format!(
                        "Per-user systemd units; export does not activate them.\nStop a manually started server using this config before enabling the service.\nCopy the units to ~/.config/systemd/user/, then run:\nsystemctl --user daemon-reload\nsystemctl --user enable --now {name}.service\nStatus: systemctl --user status {name}.service\nLogs: journalctl --user -u {name}.service\nStop: systemctl --user disable --now {name}.service\nOptional OS trigger: systemctl --user enable --now {name}-due.timer\nThe internal server scheduler already dispatches; the optional timer is safe alongside it. The server must be running for run-due. Login/linger policy is controlled by the operator.\n"
                    ),
                ),
            ])
        }
        "cron" => {
            let quote =
                |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'")).replace('%', "\\%");
            Ok(vec![(format!("{name}.crontab"),format!("* * * * * {} --config {} run-due\n",quote(binary),quote(config))),
            ("README.txt".into(),"Review and append this entry using crontab -e; do not replace existing entries. The server must already be running. This trigger does not activate the server, change user permissions or guarantee wake from sleep.\n".into())])
        }
        "windows" => {
            let args = format!("--config {} run-due", windows_arg(config));
            Ok(vec![
                (
                    format!("{name}.xml"),
                    format!(
                        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<Triggers><CalendarTrigger><Repetition><Interval>PT1M</Interval><Duration>P1D</Duration><StopAtDurationEnd>false</StopAtDurationEnd></Repetition><StartBoundary>2020-01-01T00:00:00</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger></Triggers>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT5M</ExecutionTimeLimit></Settings>
<Actions><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>
"#,
                        xml(binary),
                        xml(&args),
                        xml(cwd)
                    ),
                ),
                (
                    "README.txt".into(),
                    format!(
                        "Import {name}.xml in Task Scheduler under your chosen user. Review identity and permissions before activation. Start the Preflight server separately. Export does not register a task or store a password. OS activation requires Windows acceptance.\n"
                    ),
                ),
            ])
        }
        _ => bail!("unsupported service adapter"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_exports_escape_commands_and_keep_identity_out_of_definitions() {
        let launchd = render_worker(
            "launchd",
            "worker-test",
            "/tmp/A & B/bin",
            "http://127.0.0.1:4171",
            "/tmp/state <worker>",
        )
        .unwrap();
        assert!(launchd[0].1.contains("/tmp/A &amp; B/bin"));
        assert!(launchd[0].1.contains("/tmp/state &lt;worker&gt;"));
        assert!(
            launchd[0]
                .1
                .contains("<string>worker</string><string>connect</string>")
        );
        assert!(launchd[0].1.contains("<key>KeepAlive</key><true/>"));
        let systemd = render_worker(
            "systemd",
            "worker-test",
            "/tmp/$HOME/%n/bin",
            "http://127.0.0.1:4171",
            "/tmp/state %n",
        )
        .unwrap();
        assert!(systemd[0].1.contains(r#"ExecStart=:"/tmp/$HOME/%%n/bin" worker connect "http://127.0.0.1:4171" "/tmp/state %%n""#));
        assert!(systemd[0].1.contains("WorkingDirectory=/tmp/state %%n/\n"));
        assert!(
            systemd[0]
                .1
                .contains("StandardInput=null\nRestart=always\n")
        );
        assert!(render_worker("cron", "worker-test", "bin", "url", "state").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn worker_service_requires_private_enrolled_matching_identity() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let destination = root.join("export");
        let binary = std::env::current_exe().unwrap();
        let gateway = "http://127.0.0.1:4171";
        assert!(export_worker("launchd", &binary, gateway, &state, &destination).is_err());
        let identity = state.join("identity.json");
        let mut saved = serde_json::json!({"gateway":format!("{gateway}/"),"credential":"private-test-credential","worker_id":null,"sequence":1});
        fs::write(&identity, saved.to_string()).unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(export_worker("launchd", &binary, gateway, &state, &destination).is_err());
        saved["worker_id"] = serde_json::json!("enrolled");
        fs::write(&identity, saved.to_string()).unwrap();
        assert!(
            export_worker(
                "launchd",
                &binary,
                "http://127.0.0.1:9999",
                &state,
                &destination
            )
            .is_err()
        );
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(export_worker("launchd", &binary, gateway, &state, &destination).is_err());
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!destination.exists());
        let result = export_worker("launchd", &binary, gateway, &state, &destination).unwrap();
        assert_eq!(result["activated"], false);
        assert!(export_worker("launchd", &binary, gateway, &state, &destination).is_err());
        for entry in fs::read_dir(&destination).unwrap() {
            assert!(
                !fs::read_to_string(entry.unwrap().path())
                    .unwrap()
                    .contains("private-test-credential")
            );
        }
        assert_eq!(fs::read_to_string(identity).unwrap(), saved.to_string());
    }

    #[test]
    fn definitions_escape_paths_without_shell_execution() {
        let files = render(
            "launchd",
            "preflight-test",
            "/tmp/A & B/bin",
            "/tmp/a<config>.toml",
            "/tmp/A & B",
        )
        .unwrap();
        assert!(files[0].1.contains("/tmp/A &amp; B/bin"));
        assert!(files[0].1.contains("a&lt;config&gt;.toml"));
        assert!(files[1].1.contains("<integer>60</integer>"));
        let files = render(
            "systemd",
            "preflight-test",
            "/tmp/$HOME/%n/\"bin",
            "/tmp/a b.toml",
            "/tmp/a b",
        )
        .unwrap();
        assert!(
            files[0]
                .1
                .contains(r#"ExecStart=:"/tmp/$HOME/%%n/\"bin" --config "/tmp/a b.toml" serve"#)
        );
        assert!(files[2].1.contains("Unit=preflight-test-due.service"));
        assert!(files[0].1.contains("WorkingDirectory=/tmp/a b/\n"));
        assert!(text(Path::new("/tmp/line\nbreak")).is_err());
    }
    #[test]
    fn cron_and_windows_exports_quote_paths_and_do_not_activate() {
        let cron = render("cron", "test", "/a b/it's%bin", "/config path.toml", "/a b").unwrap();
        assert!(cron[0].1.starts_with("* * * * * "));
        assert!(cron[0].1.contains("\\%"));
        assert!(cron[0].1.contains("'\"'\"'"));
        let windows = render(
            "windows",
            "test",
            r"C:\A & B\preflight.exe",
            r"C:\A B\config.toml",
            r"C:\A B",
        )
        .unwrap();
        assert!(windows[0].1.contains("A &amp; B"));
        assert!(windows[0].1.contains("&quot;C:\\A B\\config.toml&quot;"));
        assert!(
            windows[0]
                .1
                .contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>")
        );
        assert!(windows[1].1.contains("does not register"));
    }
    #[test]
    fn export_refuses_overwrite_and_does_not_activate() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        fs::write(
            &config,
            "version=1\nbind='127.0.0.1:4170'\ndata_dir='data'\nallowed_roots=['.']\n",
        )
        .unwrap();
        let destination = temp.path().join("units");
        let binary = std::env::current_exe().unwrap();
        let result = export("launchd", &binary, &config, &destination).unwrap();
        assert_eq!(result["activated"], false);
        assert!(export("launchd", &binary, &config, &destination).is_err());
        assert!(!temp.path().join("data").exists());
    }
}
