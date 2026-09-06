// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use harness_metrics::CodeBurnSnapshot;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const DEFAULT_EXECUTABLE: &str = "codeburn";
const DEFAULT_PERIOD: &str = "30days";
const MAX_JSON_BYTES: usize = 10 * 1024 * 1024;

/// Consent-controlled runtime evidence source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    /// No process launch or snapshot read.
    #[default]
    Off,
    /// Run CodeBurn aggregate commands at startup and explicit refresh.
    Live,
    /// Read one canonical aggregate snapshot without launching a process.
    Snapshot,
}

/// Current runtime evidence availability.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    /// Runtime evidence is disabled.
    #[default]
    Off,
    /// Runtime evidence is being refreshed.
    Loading,
    /// Current aggregate evidence is available.
    Ready,
    /// Refresh failed; a previous valid snapshot may remain available.
    Failed,
    /// Configuration was rejected before any I/O or process launch.
    Invalid,
}

/// Stable, content-free runtime failure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIssue {
    /// Optional provider selection contained an unknown local ID.
    InvalidProvider,
    /// Provider initialization options could not be parsed safely.
    InvalidProviderConfiguration,
    /// Workspace trust or virtual-filesystem policy blocked optional access.
    WorkspaceBlocked,
    /// Mode was not `off`, `live`, or `snapshot`.
    InvalidMode,
    /// Period contained unsupported characters or exceeded its bound.
    InvalidPeriod,
    /// Snapshot mode lacked a configured path.
    MissingSnapshotPath,
    /// Aggregate JSON exceeded the fixed input bound.
    SnapshotTooLarge,
    /// Runtime executable does not exist.
    NotFound,
    /// Runtime executable could not be started for another safe-classified reason.
    Unavailable,
    /// Runtime command exceeded its time bound.
    Timeout,
    /// Runtime command returned a failure status.
    CommandFailed,
    /// Aggregate JSON was not valid UTF-8 or did not match the safe schema.
    InvalidData,
    /// Snapshot metadata or content could not be read.
    ReadFailed,
}

impl RuntimeIssue {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::InvalidProvider => "invalid optional provider selection",
            Self::InvalidProviderConfiguration => "invalid provider initialization options",
            Self::WorkspaceBlocked => "workspace policy blocks optional runtime access",
            Self::InvalidMode => "invalid runtime mode",
            Self::InvalidPeriod => "invalid CodeBurn period",
            Self::MissingSnapshotPath => "snapshot mode requires a snapshot path",
            Self::SnapshotTooLarge => "runtime JSON exceeds the 10 MiB safety bound",
            Self::NotFound => "runtime executable is unavailable",
            Self::Unavailable => "runtime source is unavailable",
            Self::Timeout => "runtime source timed out",
            Self::CommandFailed => "runtime source command failed",
            Self::InvalidData => "runtime source returned invalid aggregate JSON",
            Self::ReadFailed => "runtime snapshot could not be read",
        }
    }
}

/// Safe status embedded in the workspace report envelope.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    /// Configured runtime mode.
    pub mode: RuntimeMode,
    /// Current availability state.
    pub state: RuntimeState,
    /// Stable failure class, when refresh or configuration failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<RuntimeIssue>,
    /// Validated CodeBurn period selector.
    pub period: String,
    /// Calls represented by the active snapshot.
    pub calls: u64,
    /// Sessions represented by the active snapshot.
    pub sessions: u64,
    /// Safe warnings attached to optional aggregate sections.
    pub warning_count: usize,
    /// Whether a valid current or retained snapshot is available.
    pub has_snapshot: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeConfig {
    pub(crate) mode: RuntimeMode,
    pub(crate) executable: String,
    pub(crate) period: String,
    pub(crate) snapshot_path: Option<PathBuf>,
    pub(crate) issue: Option<RuntimeIssue>,
}

impl RuntimeConfig {
    pub(crate) fn from_env() -> Self {
        Self::parse(
            std::env::var("HARNESS_METRICS_MODE").ok().as_deref(),
            std::env::var("HARNESS_METRICS_CODEBURN_EXECUTABLE")
                .ok()
                .as_deref(),
            std::env::var("HARNESS_METRICS_CODEBURN_PERIOD")
                .ok()
                .as_deref(),
            std::env::var("HARNESS_METRICS_SNAPSHOT_PATH")
                .ok()
                .as_deref(),
        )
    }

    fn parse(
        mode: Option<&str>,
        executable: Option<&str>,
        period: Option<&str>,
        snapshot_path: Option<&str>,
    ) -> Self {
        let (mode, mut issue) = match mode.unwrap_or("off").trim().to_ascii_lowercase().as_str() {
            "off" => (RuntimeMode::Off, None),
            "live" => (RuntimeMode::Live, None),
            "snapshot" => (RuntimeMode::Snapshot, None),
            _ => (RuntimeMode::Off, Some(RuntimeIssue::InvalidMode)),
        };
        let period = period.unwrap_or(DEFAULT_PERIOD).trim().to_owned();
        if issue.is_none()
            && (period.is_empty()
                || period.len() > 64
                || !period
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        {
            issue = Some(RuntimeIssue::InvalidPeriod);
        }
        let snapshot_path = snapshot_path
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        if issue.is_none() && mode == RuntimeMode::Snapshot && snapshot_path.is_none() {
            issue = Some(RuntimeIssue::MissingSnapshotPath);
        }
        Self {
            mode,
            executable: executable
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(DEFAULT_EXECUTABLE)
                .to_owned(),
            period,
            snapshot_path,
            issue,
        }
    }

    pub(crate) fn initial_status(&self) -> RuntimeStatus {
        RuntimeStatus {
            mode: self.mode,
            state: if self.issue.is_some() {
                RuntimeState::Invalid
            } else {
                RuntimeState::Off
            },
            issue: self.issue,
            period: self.period.clone(),
            ..RuntimeStatus::default()
        }
    }
}

pub(crate) async fn load(config: &RuntimeConfig) -> Result<CodeBurnSnapshot, RuntimeIssue> {
    if let Some(issue) = config.issue {
        return Err(issue);
    }
    match config.mode {
        RuntimeMode::Off => Err(RuntimeIssue::Unavailable),
        RuntimeMode::Live => capture(&config.executable, &config.period).await,
        RuntimeMode::Snapshot => {
            read_snapshot(
                config
                    .snapshot_path
                    .as_deref()
                    .ok_or(RuntimeIssue::MissingSnapshotPath)?,
            )
            .await
        }
    }
}

async fn capture(executable: &str, period: &str) -> Result<CodeBurnSnapshot, RuntimeIssue> {
    let report_arguments = ["report", "--period", period, "--format", "json"];
    let model_arguments = [
        "models",
        "--period",
        period,
        "--format",
        "json",
        "--min-cost",
        "0",
    ];
    let optimize_arguments = ["optimize", "--period", period, "--json"];
    let (report, models, optimize) = tokio::join!(
        run_json(executable, &report_arguments),
        run_json(executable, &model_arguments),
        run_json(executable, &optimize_arguments),
    );
    let report = report?;
    let models = models?;
    let (optimize, warning) = match optimize {
        Ok(value) => (Some(value), false),
        Err(_) => (None, true),
    };
    let mut snapshot = CodeBurnSnapshot::from_json(&report, &models, optimize.as_deref())
        .map_err(|_| RuntimeIssue::InvalidData)?;
    if warning {
        snapshot
            .warnings
            .push("CodeBurn optimize unavailable; optimization insights omitted.".to_owned());
    }
    Ok(snapshot)
}

async fn read_snapshot(path: &Path) -> Result<CodeBurnSnapshot, RuntimeIssue> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(classify_read_error)?;
    if metadata.len() > MAX_JSON_BYTES as u64 {
        return Err(RuntimeIssue::SnapshotTooLarge);
    }
    if !metadata.is_file() {
        return Err(RuntimeIssue::ReadFailed);
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(classify_read_error)?;
    let value = read_bounded(file).await?;
    CodeBurnSnapshot::parse(&value).map_err(|_| RuntimeIssue::InvalidData)
}

fn classify_read_error(error: std::io::Error) -> RuntimeIssue {
    if error.kind() == std::io::ErrorKind::NotFound {
        RuntimeIssue::NotFound
    } else {
        RuntimeIssue::ReadFailed
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<String, RuntimeIssue> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| RuntimeIssue::ReadFailed)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(RuntimeIssue::SnapshotTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| RuntimeIssue::InvalidData)
}

async fn run_json(executable: &str, arguments: &[&str]) -> Result<String, RuntimeIssue> {
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RuntimeIssue::NotFound
            } else {
                RuntimeIssue::Unavailable
            }
        })?;
    let stdout = child.stdout.take().ok_or(RuntimeIssue::Unavailable)?;
    tokio::time::timeout(Duration::from_secs(120), async {
        let value = read_bounded(stdout).await?;
        let status = child.wait().await.map_err(|_| RuntimeIssue::Unavailable)?;
        if !status.success() {
            return Err(RuntimeIssue::CommandFailed);
        }
        Ok(value)
    })
    .await
    .map_err(|_| RuntimeIssue::Timeout)?
}

pub(crate) fn is_metrics_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        name,
        "AGENTS.md" | "CLAUDE.md" | "GEMINI.md" | "copilot-instructions.md" | "SKILL.md"
    ) {
        return true;
    }
    let normalized = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    matches!(
        normalized.as_str(),
        ".codex/config.toml"
            | ".claude/settings.json"
            | ".claude/settings.local.json"
            | ".cursor/mcp.json"
            | "opencode.json"
            | "opencode.jsonc"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn snapshot_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "harness-lens-runtime-{name}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn bounds_streams_before_collecting_unlimited_output() {
        assert_eq!(
            read_bounded(tokio::io::repeat(b'x')).await,
            Err(RuntimeIssue::SnapshotTooLarge)
        );
        assert_eq!(
            read_bounded(&b"\xff"[..]).await,
            Err(RuntimeIssue::InvalidData)
        );
        assert_eq!(read_bounded(&b"{}"[..]).await.unwrap(), "{}");
    }

    #[test]
    fn defaults_to_runtime_off() {
        let config = RuntimeConfig::parse(None, None, None, None);
        assert_eq!(config.mode, RuntimeMode::Off);
        assert_eq!(config.initial_status().state, RuntimeState::Off);
    }

    #[test]
    fn rejects_invalid_mode_and_period_without_running_processes() {
        let mode = RuntimeConfig::parse(Some("automatic"), None, None, None);
        assert_eq!(mode.issue, Some(RuntimeIssue::InvalidMode));
        let period = RuntimeConfig::parse(Some("live"), None, Some("30 days; rm"), None);
        assert_eq!(period.issue, Some(RuntimeIssue::InvalidPeriod));
    }

    #[test]
    fn snapshot_mode_requires_path() {
        let config = RuntimeConfig::parse(Some("snapshot"), None, None, None);
        assert_eq!(config.issue, Some(RuntimeIssue::MissingSnapshotPath));
    }

    #[test]
    fn recognizes_runtime_context_documents() {
        assert!(is_metrics_path(Path::new(".agents/skills/review/SKILL.md")));
        assert!(is_metrics_path(Path::new(".codex/config.toml")));
        assert!(!is_metrics_path(Path::new("src/lib.rs")));
    }

    #[tokio::test]
    async fn snapshot_mode_reads_canonical_aggregate_without_process_launch() {
        let path = snapshot_path("valid");
        std::fs::write(
            &path,
            r#"{"report":{"overview":{"calls":12,"sessions":3}},"models":[]}"#,
        )
        .unwrap();
        let config = RuntimeConfig::parse(
            Some("snapshot"),
            Some("must-not-run"),
            Some("30days"),
            path.to_str(),
        );

        let snapshot = load(&config).await.unwrap();

        assert_eq!(snapshot.report.overview.calls, 12);
        assert_eq!(snapshot.report.overview.sessions, 3);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn snapshot_mode_rejects_oversized_input_before_reading() {
        let path = snapshot_path("large");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_JSON_BYTES as u64 + 1).unwrap();
        let config = RuntimeConfig::parse(Some("snapshot"), None, None, path.to_str());

        assert_eq!(load(&config).await, Err(RuntimeIssue::SnapshotTooLarge));

        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn missing_snapshot_uses_safe_not_found_class() {
        let path = snapshot_path("missing");
        let config = RuntimeConfig::parse(Some("snapshot"), None, None, path.to_str());

        assert_eq!(load(&config).await, Err(RuntimeIssue::NotFound));
    }
}
