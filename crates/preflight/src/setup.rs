//! Setup and the main command share one POSIX process environment and timeout.
use crate::{
    model::CommandSpec,
    pipeline::{Executor, StageDefinition},
};
use anyhow::{Context, Result, ensure};
use std::{io::Write, path::Path};

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn word(value: &str, root: &Path) -> Result<String> {
    if let Ok(relative) = Path::new(value).strip_prefix(root) {
        let relative = relative.to_str().context("setup path is not UTF-8")?;
        return Ok(format!("\"${{PREFLIGHT_WORKSPACE}}\"/{}", quote(relative)));
    }
    Ok(quote(value))
}
fn invocation(program: &str, args: &[String], root: &Path) -> Result<String> {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|v| word(v, root))
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join(" "))
}
pub(crate) fn prepare(
    stage: &StageDefinition,
    root: &Path,
    cwd: &Path,
    command: &mut CommandSpec,
) -> Result<()> {
    if stage.setup.is_empty() {
        return Ok(());
    }
    if matches!(stage.executor, Executor::Native) && command.program.contains(['/', '\\']) {
        let path = cwd.join(&command.program).canonicalize().context(
            "setup main program: use a bare name and PATH for tools installed during setup",
        )?;
        ensure!(
            path.starts_with(root),
            "setup main executable escapes stage workspace"
        );
    }
    let mut script = String::from(
        "#!/bin/sh\nset -e\nreadonly PREFLIGHT_WORKSPACE\n_preflight_stage_cwd=$(pwd -P)\nreadonly _preflight_stage_cwd\n_preflight_setup_step=0\ntrap '_preflight_status=$?; if [ \"$_preflight_status\" -gt 0 ] && [ \"$_preflight_status\" -lt 128 ] && [ \"$_preflight_setup_step\" -gt 0 ]; then printf \"Environment setup step %s failed (exit %s)\\n\" \"$_preflight_setup_step\" \"$_preflight_status\" >&2; fi' EXIT\n",
    );
    for (index, step) in stage.setup.iter().enumerate() {
        script.push_str(&format!(
            "_preflight_setup_step={}\n{}\ncd \"$_preflight_stage_cwd\"\n",
            index + 1,
            invocation(&step.program, &step.args, root)?
        ));
    }
    script.push_str(&format!(
        "_preflight_setup_step=0\nexec {}\n",
        invocation(&command.program, &command.args, root)?
    ));
    let directory = root.join(".preflight");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("setup-stage.sh");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if matches!(stage.executor, Executor::Docker) {
            0o644
        } else {
            0o600
        });
    }
    let mut file = options.open(&path)?;
    file.write_all(script.as_bytes())?;
    file.sync_all()?;
    command.program = "sh".into();
    command.args = vec![path.to_str().context("setup path is not UTF-8")?.into()];
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoting_keeps_literals_and_rebases_only_whole_workspace_paths() {
        assert_eq!(quote("x'$(touch bad)"), "'x'\\''$(touch bad)'");
        assert_eq!(
            word("/work/.preflight/inputs/a/tool", Path::new("/work")).unwrap(),
            "\"${PREFLIGHT_WORKSPACE}\"/'.preflight/inputs/a/tool'"
        );
        assert_eq!(
            word("--path=/work/file", Path::new("/work")).unwrap(),
            "'--path=/work/file'"
        );
    }
    #[cfg(unix)]
    #[test]
    fn installed_tool_and_exported_environment_reach_main_without_argument_interpolation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut stage:StageDefinition=serde_json::from_value(serde_json::json!({"id":"test","setup":[{"program":".","args":["./setup.sh"]}],"command":{"program":"new-tool","args":["$(touch should-not-exist)","a'b"]}})).unwrap();
        std::fs::write(
            root.join("setup.sh"),
            r#"mkdir tools
cat > tools/new-tool <<'TOOL'
#!/bin/sh
printf '%s\n' "$EXPORTED" "$1" "$2"
TOOL
chmod +x tools/new-tool
export PATH="$PREFLIGHT_WORKSPACE/tools:$PATH"
export EXPORTED=installed
"#,
        )
        .unwrap();
        let mut command = stage.command.clone();
        prepare(&stage, &root, &root, &mut command).unwrap();
        let result = std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&root)
            .env("PREFLIGHT_WORKSPACE", &root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            "installed\n$(touch should-not-exist)\na'b\n"
        );
        assert!(!root.join("should-not-exist").exists());
        stage.setup[0].program = "false".into();
        stage.setup[0].args.clear();
        std::fs::remove_file(root.join(".preflight/setup-stage.sh")).unwrap();
        let mut command = CommandSpec {
            program: "touch".into(),
            args: vec!["main-ran".into()],
            timeout_seconds: 5,
        };
        prepare(&stage, &root, &root, &mut command).unwrap();
        let result = std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&root)
            .env("PREFLIGHT_WORKSPACE", &root)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!root.join("main-ran").exists());
        assert!(String::from_utf8_lossy(&result.stderr).contains("setup step 1 failed"));
    }
}
