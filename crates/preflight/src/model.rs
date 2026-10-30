use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterProject {
    pub name: String,
    pub path: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    300
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartBuild {
    pub project_id: String,
    pub command: CommandSpec,
}
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunId {
    pub run_id: String,
}
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelRun {
    pub run_id: String,
    pub actor: Actor,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Agent,
}
impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Preparing,
    Running,
    Collecting,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    InfrastructureError,
    Blocked,
    Skipped,
}
impl RunState {
    pub fn terminal(self) -> bool {
        !matches!(
            self,
            Self::Queued | Self::Preparing | Self::Running | Self::Collecting
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Collecting => "collecting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::InfrastructureError => "infrastructure_error",
            Self::Blocked => "blocked",
            Self::Skipped => "skipped",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRun {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<serde_json::Value>,
    pub id: String,
    pub definition: crate::pipeline::StageDefinition,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub cancel_actor: Option<String>,
    pub reason: Option<String>,
    pub tests: Option<TestReport>,
    pub status: RunState,
    pub exit_code: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expired_payloads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<serde_json::Value>,
    pub id: String,
    pub project_id: String,
    pub status: RunState,
    pub command: CommandSpec,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub exit_code: Option<i32>,
    pub cancel_actor: Option<String>,
    pub reason: Option<String>,
    pub workspace: PathBuf,
    pub stage: StageRun,
    pub stages: Vec<StageRun>,
    pub pipeline: Option<crate::pipeline::PipelineFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub stage_id: String,
    pub id: i64,
    pub stream: String,
    pub sequence: i64,
    pub timestamp: i64,
    #[serde(default)]
    pub ingested_at: Option<i64>,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub events: Vec<LogEvent>,
    pub next_cursor: Option<i64>,
}
#[derive(Debug)]
pub enum Fault {
    Invalid(String),
    NotFound,
    Busy,
    Conflict,
    IdempotencyConflict,
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "{s}"),
            Self::NotFound => write!(f, "not found"),
            Self::IdempotencyConflict => {
                write!(f, "idempotency key is already bound to a different request")
            }
            Self::Conflict => write!(
                f,
                "configuration revision conflict; read latest revision and retry explicitly"
            ),
            Self::Busy => write!(f, "active run limit reached"),
        }
    }
}
impl std::error::Error for Fault {}
pub fn invalid(message: impl Into<String>) -> anyhow::Error {
    Fault::Invalid(message.into()).into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub class_name: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<TestOutput>,
    pub id: String,
    pub name: String,
    pub class_name: String,
    pub suite: String,
    pub status: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub duration_seconds: Option<f64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestOutput {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestReport {
    #[serde(default)]
    pub cases: Vec<TestCase>,
    #[serde(default)]
    pub cases_complete: bool,
    pub counts: TestCounts,
    pub failures: Vec<TestFailure>,
    pub truncated: bool,
}
impl TestReport {
    pub fn merge(&mut self, other: Self) {
        let empty = self.counts.passed + self.counts.failed + self.counts.skipped == 0
            && self.cases.is_empty();
        self.cases_complete = (empty || self.cases_complete)
            && other.cases_complete
            && self.cases.len() + other.cases.len() <= 10000;
        self.cases.extend(other.cases);
        self.cases.truncate(10000);
        self.counts.passed += other.counts.passed;
        self.counts.failed += other.counts.failed;
        self.counts.skipped += other.counts.skipped;
        self.truncated |= other.truncated || self.failures.len() + other.failures.len() > 20;
        self.failures.extend(other.failures);
        self.failures.truncate(20);
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub run_id: String,
    pub stage_id: String,
    pub name: String,
    pub kind: crate::pipeline::ArtifactKind,
    pub sha256: String,
    pub size: u64,
    pub path: PathBuf,
}
