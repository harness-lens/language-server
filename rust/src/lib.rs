// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

#![doc = include_str!("../README.md")]

mod runtime_metrics;

pub use runtime_metrics::{RuntimeIssue, RuntimeMode, RuntimeState, RuntimeStatus};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harness_lens::{
    AnalysisReport, Finding, Scanner, Severity, TextSpan, is_harness_path, load_for_root,
};
use harness_metrics::{
    CodeBurnSnapshot, DocumentInsight, InsightSeverity, InsightSpan, analyze_document_for_project,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::{Error, Result};
use tower_lsp_server::ls_types::{
    CodeLens, CodeLensOptions, CodeLensParams, Command, Diagnostic, DiagnosticRelatedInformation,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, ExecuteCommandOptions,
    ExecuteCommandParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, Location, MarkupContent, MarkupKind,
    MessageType, NumberOrString, Position, PositionEncodingKind, Range, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

const DIAGNOSTIC_SOURCE: &str = "harness-lens";
const METRICS_SOURCE: &str = "harness-metrics";
const REFRESH_RUNTIME_COMMAND: &str = "harnessMetrics.refreshCodeBurn";
const SHOW_INSIGHT_COMMAND: &str = "harnessMetrics.showInsight";
const WORKSPACE_REPORT_METHOD: &str = "harnessLens/workspaceReport";
const DEFAULT_MAX_FILES: usize = 5_000;
const ABSOLUTE_MAX_FILES: usize = 50_000;

/// Parameters for a content-safe workspace report request.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReportParams {
    /// Optional initialized workspace root. When absent, all roots are scanned.
    pub root_uri: Option<Uri>,
    /// Maximum discovered files per root. Defaults to 5,000 and cannot exceed 50,000.
    pub max_files: Option<usize>,
}

/// Versioned response carrying the same reports consumed by CLI and diagnostics.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReports {
    /// Protocol envelope version.
    pub schema_version: u32,
    /// One deterministic analysis report per requested workspace root.
    pub reports: Vec<AnalysisReport>,
    /// Optional aggregate runtime status. Runtime data never changes these reports.
    pub runtime: RuntimeStatus,
}

#[derive(Default)]
struct State {
    roots: Vec<PathBuf>,
    open_documents: BTreeMap<PathBuf, OpenDocument>,
    runtime_snapshot: Option<CodeBurnSnapshot>,
    runtime_status: runtime_metrics::RuntimeStatus,
}

#[derive(Clone)]
struct OpenDocument {
    uri: Uri,
    text: String,
}

/// Harness Lens language server backend.
pub struct Backend {
    client: Client,
    state: RwLock<State>,
    runtime_config: runtime_metrics::RuntimeConfig,
}

impl Backend {
    fn new(client: Client) -> Self {
        let runtime_config = runtime_metrics::RuntimeConfig::from_env();
        let runtime_status = runtime_config.initial_status();
        Self {
            client,
            state: RwLock::new(State {
                runtime_status,
                ..State::default()
            }),
            runtime_config,
        }
    }

    async fn refresh_runtime(&self) {
        if self.runtime_config.mode == runtime_metrics::RuntimeMode::Off {
            return;
        }
        if let Some(issue) = self.runtime_config.issue {
            let mut state = self.state.write().await;
            state.runtime_status.state = runtime_metrics::RuntimeState::Invalid;
            state.runtime_status.issue = Some(issue);
            return;
        }
        {
            let mut state = self.state.write().await;
            state.runtime_status.state = runtime_metrics::RuntimeState::Loading;
            state.runtime_status.issue = None;
        }
        match runtime_metrics::load(&self.runtime_config).await {
            Ok(snapshot) => {
                let status = runtime_metrics::RuntimeStatus {
                    mode: self.runtime_config.mode,
                    state: runtime_metrics::RuntimeState::Ready,
                    issue: None,
                    period: self.runtime_config.period.clone(),
                    calls: snapshot.report.overview.calls,
                    sessions: snapshot.report.overview.sessions,
                    warning_count: snapshot.warnings.len(),
                    has_snapshot: true,
                };
                let mut state = self.state.write().await;
                state.runtime_snapshot = Some(snapshot);
                state.runtime_status = status;
                drop(state);
                self.client
                    .log_message(
                        MessageType::INFO,
                        "Harness Metrics runtime snapshot refreshed",
                    )
                    .await;
                let _ = self.client.code_lens_refresh().await;
            }
            Err(issue) => {
                let mut state = self.state.write().await;
                state.runtime_status.state = runtime_metrics::RuntimeState::Failed;
                state.runtime_status.issue = Some(issue);
                state.runtime_status.has_snapshot = state.runtime_snapshot.is_some();
                drop(state);
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("Harness Metrics: {}", issue.message()),
                    )
                    .await;
            }
        }
    }

    async fn analyze_open_documents(&self) {
        let (roots, documents, runtime_snapshot) = {
            let state = self.state.read().await;
            (
                state.roots.clone(),
                state.open_documents.clone(),
                state.runtime_snapshot.clone(),
            )
        };
        let mut published = BTreeSet::new();

        for root in &roots {
            let documents_for_root = documents
                .iter()
                .filter(|(path, _)| root_for_path(path, &roots) == Some(root.as_path()))
                .map(|(path, document)| (path.clone(), document.text.clone()))
                .collect::<BTreeMap<_, _>>();
            if documents_for_root.is_empty() {
                continue;
            }

            let config = match load_for_root(root, None) {
                Ok(config) => config,
                Err(error) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("Harness Lens config: {error}"))
                        .await;
                    continue;
                }
            };
            let report =
                match Scanner::new().scan_with_overrides(root, &config, &documents_for_root) {
                    Ok(report) => report,
                    Err(error) => {
                        self.client
                            .log_message(MessageType::ERROR, format!("Harness Lens scan: {error}"))
                            .await;
                        continue;
                    }
                };
            if !report.completeness.complete {
                let reason_codes = report
                    .completeness
                    .reasons
                    .iter()
                    .map(|reason| reason.code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("Harness Lens scan is incomplete: {reason_codes}"),
                    )
                    .await;
            }

            for (path, content) in &documents_for_root {
                let relative = match path.strip_prefix(root) {
                    Ok(relative) => relative,
                    Err(_) => continue,
                };
                if !is_harness_path(relative, &config.discovery) {
                    continue;
                }
                let Some(uri) = documents.get(path).map(|document| document.uri.clone()) else {
                    continue;
                };
                let mut diagnostics = report
                    .findings
                    .iter()
                    .filter(|finding| finding.path.as_deref() == Some(relative))
                    .filter_map(|finding| {
                        let mut diagnostic = diagnostic_from_finding(finding, content)?;
                        diagnostic.related_information =
                            related_information(finding, root, &documents_for_root);
                        Some(diagnostic)
                    })
                    .collect::<Vec<_>>();
                if let Some(snapshot) = &runtime_snapshot {
                    diagnostics.extend(metrics_diagnostics(root, relative, content, snapshot));
                }
                self.client
                    .publish_diagnostics(uri.clone(), diagnostics, None)
                    .await;
                published.insert(path.clone());
            }
        }

        for (path, document) in documents {
            if published.contains(&path) {
                continue;
            }
            let diagnostics = runtime_snapshot
                .as_ref()
                .and_then(|snapshot| {
                    let root = root_for_path(&path, &roots)?;
                    let relative = path.strip_prefix(root).ok()?;
                    Some(metrics_diagnostics(
                        root,
                        relative,
                        &document.text,
                        snapshot,
                    ))
                })
                .unwrap_or_default();
            self.client
                .publish_diagnostics(document.uri, diagnostics, None)
                .await;
        }
    }

    async fn put_document(&self, uri: &Uri, text: String) {
        let Some(path) = uri.to_file_path() else {
            return;
        };
        let path = path.into_owned();
        let path = path.canonicalize().unwrap_or(path);
        self.state.write().await.open_documents.insert(
            path,
            OpenDocument {
                uri: uri.clone(),
                text,
            },
        );
    }

    async fn workspace_report(&self, params: WorkspaceReportParams) -> Result<WorkspaceReports> {
        let (roots, documents, runtime_status) = {
            let state = self.state.read().await;
            (
                state.roots.clone(),
                state.open_documents.clone(),
                state.runtime_status.clone(),
            )
        };
        let max_files = params.max_files.unwrap_or(DEFAULT_MAX_FILES);
        if max_files == 0 || max_files > ABSOLUTE_MAX_FILES {
            return Err(Error::invalid_params(
                "maxFiles must be between 1 and 50000",
            ));
        }
        let requested_root = params
            .root_uri
            .map(|uri| {
                uri.to_file_path()
                    .map(|path| path.into_owned())
                    .ok_or_else(|| Error::invalid_params("rootUri must be a file URI"))
            })
            .transpose()?;

        build_workspace_reports(
            &roots,
            &documents,
            requested_root.as_deref(),
            max_files,
            runtime_status,
        )
        .map_err(Error::invalid_params)
    }

    async fn metrics_document(
        &self,
        uri: &Uri,
    ) -> Option<(PathBuf, OpenDocument, Option<CodeBurnSnapshot>, PathBuf)> {
        let path = uri.to_file_path()?.into_owned();
        let path = path.canonicalize().unwrap_or(path);
        let state = self.state.read().await;
        let document = state.open_documents.get(&path)?.clone();
        let snapshot = state.runtime_snapshot.clone();
        let root = root_for_path(&path, &state.roots)?;
        let relative = path.strip_prefix(root).ok()?.to_owned();
        Some((root.to_owned(), document, snapshot, relative))
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let mut roots = params
            .workspace_folders
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().map(|path| path.into_owned()))
            .collect::<Vec<_>>();
        if roots.is_empty() {
            if let Some(root_uri) = legacy_root_uri(&params) {
                if let Some(path) = root_uri.to_file_path() {
                    roots.push(path.into_owned());
                }
            }
        }
        if roots.is_empty() {
            roots.push(
                PathBuf::from(".")
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(".")),
            );
        }
        for root in &mut roots {
            *root = root.canonicalize().unwrap_or_else(|_| root.clone());
        }
        roots.sort();
        roots.dedup();
        self.state.write().await.roots = roots;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        REFRESH_RUNTIME_COMMAND.to_owned(),
                        SHOW_INSIGHT_COMMAND.to_owned(),
                    ],
                    ..ExecuteCommandOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "Harness Lens".to_owned(),
                version: Some(harness_lens::VERSION.to_owned()),
            }),
            offset_encoding: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Harness Lens language server ready")
            .await;
        self.refresh_runtime().await;
        self.analyze_open_documents().await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.put_document(&params.text_document.uri, params.text_document.text)
            .await;
        self.analyze_open_documents().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.put_document(&params.text_document.uri, change.text)
                .await;
            self.analyze_open_documents().await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.put_document(&params.text_document.uri, text).await;
        }
        self.analyze_open_documents().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        if let Some(path) = params.text_document.uri.to_file_path() {
            let path = path.into_owned();
            let path = path.canonicalize().unwrap_or(path);
            self.state.write().await.open_documents.remove(&path);
        }
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
        self.analyze_open_documents().await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((root, document, snapshot, relative)) = self.metrics_document(&uri).await else {
            return Ok(None);
        };
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        if !runtime_metrics::is_metrics_path(&relative) {
            return Ok(None);
        }
        let Some(byte) = byte_at_position(&document.text, position) else {
            return Ok(None);
        };
        let insights =
            analyze_document_for_project(&relative, &document.text, &snapshot, Some(&root));
        let Some(insight) = insights.iter().find(|insight| {
            insight
                .span
                .is_some_and(|span| span.start <= byte && byte < span.end)
        }) else {
            return Ok(None);
        };
        let range = insight
            .span
            .and_then(|span| range_from_insight_span(&document.text, span));
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "### {}\n\n{}\n\n_Source: CodeBurn via Harness Metrics · Method: {}_",
                    insight.title, insight.detail, insight.method
                ),
            }),
            range,
        }))
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let Some((root, document, snapshot, relative)) =
            self.metrics_document(&params.text_document.uri).await
        else {
            return Ok(None);
        };
        if !runtime_metrics::is_metrics_path(&relative) {
            return Ok(None);
        }
        Ok(Some(metrics_code_lenses(
            &root,
            &relative,
            &document.text,
            snapshot.as_ref(),
        )))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<tower_lsp_server::ls_types::LSPAny>> {
        match params.command.as_str() {
            REFRESH_RUNTIME_COMMAND => {
                self.refresh_runtime().await;
                self.analyze_open_documents().await;
                Ok(None)
            }
            SHOW_INSIGHT_COMMAND => Ok(None),
            _ => Err(Error::invalid_request()),
        }
    }
}

#[allow(deprecated)]
fn legacy_root_uri(params: &InitializeParams) -> Option<Uri> {
    params.root_uri.clone()
}

/// Serves Harness Lens LSP over standard input/output.
pub async fn serve() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::build(Backend::new)
        .custom_method(WORKSPACE_REPORT_METHOD, Backend::workspace_report)
        .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}

fn build_workspace_reports(
    roots: &[PathBuf],
    documents: &BTreeMap<PathBuf, OpenDocument>,
    requested_root: Option<&Path>,
    max_files: usize,
    runtime: runtime_metrics::RuntimeStatus,
) -> std::result::Result<WorkspaceReports, String> {
    let requested_root =
        requested_root.map(|root| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()));
    if let Some(requested) = &requested_root {
        if !roots.iter().any(|root| root == requested) {
            return Err(format!(
                "rootUri is not an initialized workspace root: {}",
                requested.display()
            ));
        }
    }

    let mut reports = Vec::new();
    for root in roots.iter().filter(|root| {
        requested_root
            .as_ref()
            .is_none_or(|requested| *root == requested)
    }) {
        let mut config = load_for_root(root, None)
            .map_err(|error| format!("cannot load Harness Lens config: {error}"))?;
        config.discovery.max_files = config.discovery.max_files.min(max_files);
        let overrides = documents
            .iter()
            .filter(|(path, _)| root_for_path(path, roots) == Some(root.as_path()))
            .map(|(path, document)| (path.clone(), document.text.clone()))
            .collect::<BTreeMap<_, _>>();
        let report = Scanner::new()
            .scan_with_overrides(root, &config, &overrides)
            .map_err(|error| format!("cannot scan workspace: {error}"))?;
        reports.push(report);
    }

    Ok(WorkspaceReports {
        schema_version: 1,
        reports,
        runtime,
    })
}

fn diagnostic_from_finding(finding: &Finding, content: &str) -> Option<Diagnostic> {
    if finding.severity == Severity::Pass {
        return None;
    }
    let range = finding
        .span
        .and_then(|span| range_from_byte_span(content, span))
        .or_else(|| finding.line.map(|line| whole_line_range(content, line)))
        .unwrap_or_default();
    Some(Diagnostic {
        range,
        severity: Some(match finding.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Info => DiagnosticSeverity::INFORMATION,
            Severity::Pass => return None,
        }),
        code: Some(NumberOrString::String(finding.rule_id.clone())),
        source: Some(DIAGNOSTIC_SOURCE.to_owned()),
        message: match finding.evidence.as_deref() {
            Some(evidence) => format!("{}\n\nEvidence: {evidence}", finding.message),
            None => finding.message.clone(),
        },
        ..Diagnostic::default()
    })
}

fn metrics_diagnostics(
    root: &Path,
    path: &Path,
    content: &str,
    snapshot: &CodeBurnSnapshot,
) -> Vec<Diagnostic> {
    if !runtime_metrics::is_metrics_path(path) {
        return Vec::new();
    }
    analyze_document_for_project(path, content, snapshot, Some(root))
        .into_iter()
        .filter(|insight| insight.severity == InsightSeverity::Warning)
        .map(|insight| Diagnostic {
            range: insight
                .span
                .and_then(|span| range_from_insight_span(content, span))
                .unwrap_or_default(),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String(insight.code)),
            source: Some(METRICS_SOURCE.to_owned()),
            message: format!(
                "{}: {} (method: {})",
                insight.title, insight.detail, insight.method
            ),
            ..Diagnostic::default()
        })
        .collect()
}

fn code_lens_from_insight(insight: DocumentInsight) -> CodeLens {
    CodeLens {
        range: Range::default(),
        command: Some(Command {
            title: format!(
                "[{}] {} — {}",
                insight.method, insight.title, insight.detail
            ),
            command: SHOW_INSIGHT_COMMAND.to_owned(),
            arguments: None,
        }),
        data: None,
    }
}

fn metrics_code_lenses(
    root: &Path,
    path: &Path,
    content: &str,
    snapshot: Option<&CodeBurnSnapshot>,
) -> Vec<CodeLens> {
    let mut lenses = snapshot
        .map(|snapshot| {
            analyze_document_for_project(path, content, snapshot, Some(root))
                .into_iter()
                .filter(|insight| insight.span.is_none() || insight.code == "HM200")
                .map(code_lens_from_insight)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    lenses.push(CodeLens {
        range: Range::default(),
        command: Some(Command {
            title: "Refresh Harness Metrics runtime snapshot".to_owned(),
            command: REFRESH_RUNTIME_COMMAND.to_owned(),
            arguments: None,
        }),
        data: None,
    });
    lenses
}

fn related_information(
    finding: &Finding,
    root: &Path,
    documents: &BTreeMap<PathBuf, String>,
) -> Option<Vec<DiagnosticRelatedInformation>> {
    let locations = finding
        .related
        .iter()
        .filter_map(|related| {
            let path = root.join(&related.path);
            let uri = file_uri(&path)?;
            // Unsaved editor content takes precedence over the on-disk source.
            let content = documents
                .get(&path)
                .cloned()
                .or_else(|| std::fs::read_to_string(&path).ok());
            let range = content
                .as_deref()
                .and_then(|content| {
                    related
                        .span
                        .and_then(|span| range_from_byte_span(content, span))
                        .or_else(|| related.line.map(|line| whole_line_range(content, line)))
                })
                .unwrap_or_else(|| {
                    let position =
                        Position::new(related.line.unwrap_or(1).saturating_sub(1) as u32, 0);
                    Range::new(position, position)
                });
            Some(DiagnosticRelatedInformation {
                location: Location::new(uri, range),
                message: format!("Related instruction for {}", finding.rule_id),
            })
        })
        .collect::<Vec<_>>();
    if locations.is_empty() {
        None
    } else {
        Some(locations)
    }
}

fn file_uri(path: &Path) -> Option<Uri> {
    // canonicalize() returns extended-length paths on Windows, which must not
    // leak into LSP file URIs as encoded "?/" segments.
    #[cfg(windows)]
    let normalized = {
        let text = path.to_str()?;
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            path.to_path_buf()
        }
    };
    #[cfg(windows)]
    let path = normalized.as_path();
    Uri::from_file_path(path)
}

fn range_from_byte_span(content: &str, span: TextSpan) -> Option<Range> {
    if span.start > span.end
        || span.end > content.len()
        || !content.is_char_boundary(span.start)
        || !content.is_char_boundary(span.end)
    {
        return None;
    }
    Some(Range::new(
        position_at_byte(content, span.start),
        position_at_byte(content, span.end),
    ))
}

fn range_from_insight_span(content: &str, span: InsightSpan) -> Option<Range> {
    range_from_byte_span(
        content,
        TextSpan {
            start: span.start,
            end: span.end,
        },
    )
}

fn whole_line_range(content: &str, one_based_line: usize) -> Range {
    let target = one_based_line.saturating_sub(1);
    let line = content.lines().nth(target).unwrap_or("");
    Range::new(
        Position::new(target as u32, 0),
        Position::new(target as u32, utf16_len(line)),
    )
}

fn position_at_byte(content: &str, byte: usize) -> Position {
    let prefix = &content[..byte];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Position::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        utf16_len(&content[line_start..byte]),
    )
}

fn byte_at_position(content: &str, position: Position) -> Option<usize> {
    let line_start = if position.line == 0 {
        0
    } else {
        content
            .match_indices('\n')
            .nth(position.line.saturating_sub(1) as usize)?
            .0
            + 1
    };
    let line_end = content[line_start..]
        .find('\n')
        .map_or(content.len(), |offset| line_start + offset);
    let mut utf16 = 0_u32;
    for (offset, character) in content[line_start..line_end].char_indices() {
        if utf16 == position.character {
            return Some(line_start + offset);
        }
        utf16 += character.len_utf16() as u32;
        if utf16 > position.character {
            return None;
        }
    }
    (utf16 == position.character).then_some(line_end)
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count().try_into().unwrap_or(u32::MAX)
}

fn root_for_path<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .map(PathBuf::as_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harness-lens-lsp-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temporary workspace");
        root
    }

    #[test]
    fn workspace_report_exposes_per_file_metrics_without_source_content() {
        let root = temporary_workspace("report");
        let secret = "Never serialize SECRET_SENTINEL source content.\n";
        std::fs::write(root.join("AGENTS.md"), secret).expect("write harness source");
        let root = root.canonicalize().expect("canonical workspace");

        let response = build_workspace_reports(
            std::slice::from_ref(&root),
            &BTreeMap::new(),
            Some(&root),
            DEFAULT_MAX_FILES,
            runtime_metrics::RuntimeStatus::default(),
        )
        .expect("workspace report");

        assert_eq!(response.schema_version, 1);
        assert_eq!(response.reports.len(), 1);
        let report = &response.reports[0];
        assert!(
            report
                .sources
                .iter()
                .any(|source| source.path == Path::new("AGENTS.md"))
        );
        assert!(report.metrics.iter().any(|metric| {
            metric.path.as_deref() == Some(Path::new("AGENTS.md"))
                && metric.name == "harness.source.estimated_tokens"
        }));
        let serialized = serde_json::to_string(&response).expect("serialize response");
        assert!(!serialized.contains("SECRET_SENTINEL"));

        std::fs::remove_dir_all(root).expect("remove temporary workspace");
    }

    #[test]
    fn workspace_report_rejects_uninitialized_root() {
        let root = temporary_workspace("known");
        let other = temporary_workspace("unknown");
        let result = build_workspace_reports(
            std::slice::from_ref(&root),
            &BTreeMap::new(),
            Some(other.as_path()),
            DEFAULT_MAX_FILES,
            runtime_metrics::RuntimeStatus::default(),
        );

        assert!(
            result
                .unwrap_err()
                .contains("not an initialized workspace root")
        );
        std::fs::remove_dir_all(root).expect("remove known workspace");
        std::fs::remove_dir_all(other).expect("remove unknown workspace");
    }

    #[test]
    fn workspace_report_applies_file_bound_and_exposes_runtime_status() {
        let root = temporary_workspace("bounded");
        std::fs::write(root.join("AGENTS.md"), "first").expect("write first source");
        std::fs::write(root.join("CLAUDE.md"), "second").expect("write second source");
        let root = root.canonicalize().expect("canonical workspace");
        let runtime = RuntimeStatus {
            mode: RuntimeMode::Snapshot,
            state: RuntimeState::Ready,
            period: "30days".to_owned(),
            calls: 12,
            sessions: 3,
            has_snapshot: true,
            ..RuntimeStatus::default()
        };

        let response = build_workspace_reports(
            std::slice::from_ref(&root),
            &BTreeMap::new(),
            Some(&root),
            1,
            runtime,
        )
        .expect("bounded workspace report");

        assert_eq!(response.reports[0].sources.len(), 1);
        assert!(!response.reports[0].completeness.complete);
        assert_eq!(
            response.reports[0].completeness.reasons[0].code,
            "file-count-limit"
        );
        let serialized = serde_json::to_value(response).expect("serialize response");
        assert_eq!(serialized["runtime"]["mode"], "snapshot");
        assert_eq!(serialized["runtime"]["calls"], 12);

        std::fs::remove_dir_all(root).expect("remove bounded workspace");
    }

    #[test]
    fn byte_spans_convert_to_utf16_positions() {
        let content = "😀 use use\n";
        let range = range_from_byte_span(content, TextSpan { start: 9, end: 12 }).unwrap();

        assert_eq!(range, Range::new(Position::new(0, 7), Position::new(0, 10)));
    }

    #[test]
    fn finding_becomes_stable_standard_diagnostic() {
        let finding = Finding {
            severity: Severity::Warning,
            rule_id: "HL010".to_owned(),
            message: "Adjacent word repetition".to_owned(),
            path: Some(PathBuf::from("AGENTS.md")),
            line: Some(1),
            span: Some(TextSpan { start: 4, end: 7 }),
            evidence: None,
            source: "harness-lens.repetition".to_owned(),
            related: Vec::new(),
        };

        let diagnostic = diagnostic_from_finding(&finding, "Use use tests").unwrap();

        assert_eq!(diagnostic.source.as_deref(), Some("harness-lens"));
        assert_eq!(
            diagnostic.code,
            Some(NumberOrString::String("HL010".to_owned()))
        );
        assert_eq!(diagnostic.range.start, Position::new(0, 4));
    }

    #[test]
    fn redundancy_finding_highlights_the_full_instruction() {
        let content =
            "Try to avoid using branch names like codex.\nDo not use branches like codex.\n";
        let second_line_start = content.find("Do not").unwrap();
        let finding = Finding {
            severity: Severity::Warning,
            rule_id: "HL030".to_owned(),
            message: "Instruction repeats earlier intent at AGENTS.md:1".to_owned(),
            path: Some(PathBuf::from("AGENTS.md")),
            line: Some(2),
            span: Some(TextSpan {
                start: second_line_start,
                end: content.trim_end().len(),
            }),
            evidence: None,
            source: "harness-lens.redundancy".to_owned(),
            related: Vec::new(),
        };

        let diagnostic = diagnostic_from_finding(&finding, content).unwrap();

        assert_eq!(
            diagnostic.code,
            Some(NumberOrString::String("HL030".to_owned()))
        );
        assert_eq!(
            diagnostic.range,
            Range::new(Position::new(1, 0), Position::new(1, 31))
        );
    }

    #[test]
    fn deepest_workspace_root_wins() {
        let roots = [PathBuf::from("/repo"), PathBuf::from("/repo/nested")];
        assert_eq!(
            root_for_path(Path::new("/repo/nested/AGENTS.md"), &roots),
            Some(Path::new("/repo/nested"))
        );
    }

    #[test]
    fn related_locations_use_unsaved_content_and_utf16_ranges() {
        let root = std::env::current_dir().unwrap();
        let path = PathBuf::from("nested/AGENTS.md");
        let documents = BTreeMap::from([(root.join(&path), "😀 use tests\n".to_owned())]);
        let finding = Finding {
            severity: Severity::Warning,
            rule_id: "HL032".to_owned(),
            message: "Duplicate".to_owned(),
            path: Some(PathBuf::from("AGENTS.md")),
            line: Some(2),
            span: None,
            evidence: Some("assumption: normalize whitespace".to_owned()),
            source: "harness-lens.exact-duplicates".to_owned(),
            related: vec![harness_lens::FindingLocation {
                path: path.clone(),
                line: Some(1),
                span: Some(TextSpan { start: 5, end: 8 }),
            }],
        };
        let locations = related_information(&finding, &root, &documents).unwrap();
        assert_eq!(
            locations[0].location.uri,
            Uri::from_file_path(root.join(path)).unwrap()
        );
        assert_eq!(
            locations[0].location.range,
            Range::new(Position::new(0, 3), Position::new(0, 6))
        );
        assert!(
            diagnostic_from_finding(&finding, "first\nsecond")
                .unwrap()
                .message
                .contains("assumption:")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_extended_paths_produce_standard_file_uris() {
        assert_eq!(
            file_uri(Path::new(r"\\?\C:\repo\AGENTS.md")),
            Uri::from_file_path(r"C:\repo\AGENTS.md")
        );
        assert_eq!(
            file_uri(Path::new(r"\\?\UNC\server\share\AGENTS.md")),
            Uri::from_file_path(r"\\server\share\AGENTS.md")
        );
    }
}
