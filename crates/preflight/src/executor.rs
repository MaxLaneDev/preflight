use crate::{
    db::Db,
    model::{CommandSpec, RunState},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

pub(crate) const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "CARGO_HOME",
    "RUSTUP_HOME",
    // Native Windows tool discovery needs these directories and linker search paths.
    #[cfg(windows)]
    "ProgramFiles",
    #[cfg(windows)]
    "ProgramFiles(x86)",
    #[cfg(windows)]
    "ProgramW6432",
    #[cfg(windows)]
    "ProgramData",
    #[cfg(windows)]
    "LOCALAPPDATA",
    #[cfg(windows)]
    "APPDATA",
    #[cfg(windows)]
    "SystemDrive",
    #[cfg(windows)]
    "LIB",
    #[cfg(windows)]
    "LIBPATH",
    #[cfg(windows)]
    "INCLUDE",
    #[cfg(windows)]
    "VSINSTALLDIR",
    #[cfg(windows)]
    "VCINSTALLDIR",
    #[cfg(windows)]
    "VCToolsInstallDir",
    #[cfg(windows)]
    "WindowsSdkDir",
    #[cfg(windows)]
    "WindowsSDKVersion",
    #[cfg(windows)]
    "UniversalCRTSdkDir",
    #[cfg(windows)]
    "UCRTVersion",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    All,
    Output,
    None,
}
pub struct Execution<'a> {
    pub container: Option<&'a crate::docker::Context>,
    pub log_mode: LogMode,
    pub verified_executable: Option<&'a crate::model::Artifact>,
    pub run_id: &'a str,
    pub stage_id: &'a str,
    pub workspace: &'a Path,
    pub cwd: &'a Path,
    pub command: &'a CommandSpec,
    pub stdin: Option<&'a [u8]>,
    pub capture_limit: usize,
    pub env: &'a BTreeMap<String, String>,
}
pub async fn execute(
    db: &Db,
    input: Execution<'_>,
    cancel: &CancellationToken,
) -> Result<(RunState, Option<i32>)> {
    let output = execute_captured(db, input, cancel).await?;
    Ok((output.status, output.exit))
}
#[derive(Debug)]
pub struct ProcessOutput {
    pub status: RunState,
    pub exit: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}
pub async fn execute_captured(
    db: &Db,
    input: Execution<'_>,
    cancel: &CancellationToken,
) -> Result<ProcessOutput> {
    if let Some(context) = input.container {
        return crate::docker::execute(db, input, context, cancel).await;
    }
    execute_captured_native(db, input, cancel).await
}
pub(crate) async fn execute_captured_native(
    db: &Db,
    input: Execution<'_>,
    cancel: &CancellationToken,
) -> Result<ProcessOutput> {
    if cancel.is_cancelled() {
        return Ok(ProcessOutput {
            status: RunState::Cancelled,
            exit: None,
            stdout: vec![],
            stderr: vec![],
            truncated: false,
        });
    }
    let executable = if input.command.program.contains(['/', '\\']) {
        let path = input
            .cwd
            .join(&input.command.program)
            .canonicalize()
            .context("resolve stage executable")?;
        if let Some(pin) = input.verified_executable {
            ensure!(pin.path == path, "verified executable path changed");
            crate::pins::verify_original(pin)?;
        } else {
            ensure!(
                path.starts_with(input.workspace),
                "executable path escapes stage workspace"
            );
        }
        path
    } else {
        PathBuf::from(&input.command.program)
    };
    let mut command = Command::new(executable);
    command
        .args(&input.command.args)
        .current_dir(input.cwd)
        .stdin(if input.stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for key in INHERITED_ENV {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .envs(input.env)
        .env("PREFLIGHT_WORKSPACE", input.workspace);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(
        windows_sys::Win32::System::Threading::CREATE_SUSPENDED
            | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
    );
    let mut child = command
        .spawn()
        .context("spawn native command (tool must be installed)")?;
    let mut tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            child.kill().await.ok();
            return Err(error.context("attach native process tree"));
        }
    };
    if input.log_mode == LogMode::All {
        // Program identity is useful for quiet commands; argv/environment may contain credentials.
        db.append_stage_log(
            input.run_id,
            input.stage_id,
            "lifecycle",
            0,
            &format!("Preflight process started: {}\n", input.command.program),
        )?;
    }
    let mut stdin = child.stdin.take();
    let feed = async {
        if let Some(pipe) = stdin.as_mut()
            && let Err(error) = pipe.write_all(input.stdin.unwrap_or_default()).await
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(error.into());
        }
        drop(stdin.take());
        Ok::<_, anyhow::Error>(())
    };
    let stdout = child.stdout.take().context("stdout pipe")?;
    let stderr = child.stderr.take().context("stderr pipe")?;
    let pipes_done = CancellationToken::new();
    #[cfg(unix)]
    let mut signal: Option<i32> = None;
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let control = async {
        let result = tokio::select! {
            biased;
            _=cancel.cancelled()=>Ok((RunState::Cancelled,None)),
            _=tokio::time::sleep(Duration::from_secs(input.command.timeout_seconds))=>Ok((RunState::TimedOut,None)),
            status=child.wait()=>{let status=status?;#[cfg(unix)]{use std::os::unix::process::ExitStatusExt;signal=status.signal();}Ok::<_,anyhow::Error>((if status.success(){RunState::Succeeded}else{RunState::Failed},status.code()))}
        };
        // The guard also terminates descendants if this future is dropped unexpectedly.
        let stopped = tree.terminate();
        if !matches!(result, Ok((RunState::Succeeded | RunState::Failed, _))) {
            child.kill().await.ok();
        }
        child.wait().await.ok();
        pipes_done.cancel();
        stopped?;
        result
    };
    let captures = async {
        tokio::select! {
            captured=async {tokio::join!(capture(db,input.run_id,input.stage_id,"stdout",stdout,input.capture_limit,input.log_mode!=LogMode::None),capture(db,input.run_id,input.stage_id,"stderr",stderr,input.capture_limit,input.log_mode!=LogMode::None))}=>{Ok::<_,anyhow::Error>((captured.0?,captured.1?))},
            _=async{pipes_done.cancelled().await;tokio::time::sleep(Duration::from_secs(2)).await;}=>anyhow::bail!("output pipes remained open after process exit")
        }
    };
    let feed = async {
        tokio::select! { result=feed=>result, _=pipes_done.cancelled()=>Ok(()) }
    };
    let (captured, result, fed) = tokio::join!(captures, control, feed);
    let ((stdout, out_truncated), (mut stderr, err_truncated)) = captured?;
    let (status, exit) = result?;
    fed?;
    if input.log_mode == LogMode::All {
        db.append_stage_log(
            input.run_id,
            input.stage_id,
            "lifecycle",
            1,
            &format!(
                "Preflight process finished: {} exit={}\n",
                status.as_str(),
                exit.map(|code| code.to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ),
        )?;
    }
    if let Some(signal) = signal {
        let message = format!("Preflight: process terminated by signal {signal}\n");
        let sequence:i64=db.connection()?.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM log_events WHERE run_id=?1 AND stage_id=?2 AND stream='stderr'",rusqlite::params![input.run_id,input.stage_id],|r|r.get(0))?;
        if input.log_mode != LogMode::None {
            db.append_stage_log(input.run_id, input.stage_id, "stderr", sequence, &message)?;
        }
        stderr.extend_from_slice(message.as_bytes());
    }
    Ok(ProcessOutput {
        status,
        exit,
        stdout,
        stderr,
        truncated: out_truncated || err_truncated,
    })
}
#[cfg(unix)]
pub(crate) struct ProcessTree {
    pid: Option<i32>,
}
#[cfg(unix)]
impl ProcessTree {
    pub(crate) fn attach(child: &Child) -> Result<Self> {
        Ok(Self {
            pid: Some(child.id().context("child pid")? as i32),
        })
    }
    pub(crate) fn interrupt(&self) -> Result<()> {
        if let Some(pid) = self.pid
            && unsafe { libc::kill(-pid, libc::SIGINT) } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }
    pub(crate) fn terminate(&mut self) -> Result<()> {
        if let Some(pid) = self.pid.take() {
            // The child was spawned as leader of its own process group.
            if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(windows)]
pub(crate) use windows_tree::ProcessTree;
#[cfg(windows)]
mod windows_tree {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
    };
    pub(crate) struct ProcessTree {
        job: OwnedHandle,
    }
    impl ProcessTree {
        pub(crate) fn attach(child: &Child) -> Result<Self> {
            // The child starts suspended, so it cannot spawn descendants before assignment.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                ensure!(!handle.is_null(), std::io::Error::last_os_error());
                let tree = Self {
                    job: OwnedHandle::from_raw_handle(handle),
                };
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                ensure!(
                    SetInformationJobObject(
                        handle,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32
                    ) != 0,
                    std::io::Error::last_os_error()
                );
                ensure!(
                    AssignProcessToJobObject(handle, child.raw_handle().context("child handle")?)
                        != 0,
                    std::io::Error::last_os_error()
                );
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                ensure!(
                    snapshot != INVALID_HANDLE_VALUE,
                    std::io::Error::last_os_error()
                );
                let snapshot = OwnedHandle::from_raw_handle(snapshot);
                let mut entry: THREADENTRY32 = std::mem::zeroed();
                entry.dwSize = std::mem::size_of_val(&entry) as u32;
                let mut found = Thread32First(snapshot.as_raw_handle(), &mut entry) != 0;
                while found {
                    if entry.th32OwnerProcessID == child.id().context("child pid")? {
                        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                        ensure!(!thread.is_null(), std::io::Error::last_os_error());
                        let thread = OwnedHandle::from_raw_handle(thread);
                        ensure!(
                            ResumeThread(thread.as_raw_handle()) != u32::MAX,
                            std::io::Error::last_os_error()
                        );
                        return Ok(tree);
                    }
                    found = Thread32Next(snapshot.as_raw_handle(), &mut entry) != 0;
                }
                anyhow::bail!("suspended process primary thread not found")
            }
        }
        pub(crate) fn terminate(&mut self) -> Result<()> {
            ensure!(
                unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } != 0,
                std::io::Error::last_os_error()
            );
            Ok(())
        }
    }
}
async fn capture(
    db: &Db,
    id: &str,
    stage_id: &str,
    stream: &str,
    mut reader: impl AsyncRead + Unpin,
    cap: usize,
    emit: bool,
) -> Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut secrets = crate::secrets::Filter::new(db.secret_values());
    let mut buffer = [0u8; 4096];
    let mut stored = 0;
    let mut sequence = 0;
    let mut pending = Vec::new();
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        let allowed = n.min(cap - stored);
        bytes.extend_from_slice(&buffer[..allowed]);
        pending.extend_from_slice(&buffer[..allowed]);
        stored += allowed;
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => pending.len(),
        };
        if valid > 0 {
            let redacted = secrets.push(&pending[..valid], false);
            if emit && !redacted.is_empty() {
                db.append_stage_log(
                    id,
                    stage_id,
                    stream,
                    sequence,
                    &String::from_utf8_lossy(&redacted),
                )?;
                sequence += 1;
            }
            pending.drain(..valid);
        }
        if allowed < n && !truncated && !emit {
            truncated = true;
        }
        if allowed < n && !truncated && emit {
            let hidden = secrets.truncate();
            if !hidden.is_empty() {
                db.append_stage_log(
                    id,
                    stage_id,
                    stream,
                    sequence,
                    &String::from_utf8_lossy(&hidden),
                )?;
                sequence += 1;
            }
            db.append_stage_log(
                id,
                stage_id,
                stream,
                sequence,
                &format!(
                    "\n[Preflight: output truncated after {}]\n",
                    if cap == 1024 * 1024 {
                        "1 MiB".into()
                    } else {
                        format!("{cap} bytes")
                    }
                ),
            )?;
            sequence += 1;
            truncated = true;
        }
    }
    let tail = secrets.push(&pending, true);
    if emit && !tail.is_empty() {
        db.append_stage_log(
            id,
            stage_id,
            stream,
            sequence,
            &String::from_utf8_lossy(&tail),
        )?;
    }
    Ok((bytes, truncated))
}
