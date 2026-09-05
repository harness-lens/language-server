// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

#![doc = include_str!("../README.md")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harness_lens::{Finding, Scanner, Severity, TextSpan, is_harness_path, load_for_root};
use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, Location, MessageType, NumberOrString,
    Position, PositionEncodingKind, Range, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

const DIAGNOSTIC_SOURCE: &str = "harness-lens";

#[derive(Default)]
struct State {
    roots: Vec<PathBuf>,
    open_documents: BTreeMap<PathBuf, OpenDocument>,
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
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            state: RwLock::new(State::default()),
        }
    }

    async fn analyze_open_documents(&self) {
        let (roots, documents) = {
            let state = self.state.read().await;
            (state.roots.clone(), state.open_documents.clone())
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
                let diagnostics = report
                    .findings
                    .iter()
                    .filter(|finding| finding.path.as_deref() == Some(relative))
                    .filter_map(|finding| {
                        let mut diagnostic = diagnostic_from_finding(finding, content)?;
                        diagnostic.related_information =
                            related_information(finding, root, &documents_for_root);
                        Some(diagnostic)
                    })
                    .collect();
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
            self.client
                .publish_diagnostics(document.uri, Vec::new(), None)
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
}

#[allow(deprecated)]
fn legacy_root_uri(params: &InitializeParams) -> Option<Uri> {
    params.root_uri.clone()
}

/// Serves Harness Lens LSP over standard input/output.
pub async fn serve() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
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
