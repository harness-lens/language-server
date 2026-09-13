// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

//! Bounded observed-flow protocol and UTF-16 source navigation mapping.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_lens::observed_flow::{FlowMetric, ObservedFlowOptions, build_observed_flow};
use harness_lens::trace::{NormalizedTrace, normalize_trace_json};
use harness_lens::{
    CompletenessReason, EvidenceCompleteness, EvidenceLocation, GraphAvailability, GraphFilters,
    GraphKind, GraphLimits, GraphNodeKind, GraphRelationship, ObservationWindow,
    RELATIONSHIP_GRAPH_SCHEMA_VERSION, RelationshipGraph, RuntimeObservationStatus, ScoreMethod,
    WeightedEdgeMetric,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tower_lsp_server::ls_types::{Location, Uri};

use crate::runtime_metrics::RuntimeMode;
use crate::{file_uri, range_from_byte_span, whole_line_range};

pub(crate) const OBSERVED_FLOW_METHOD: &str = "harnessLens/observedFlow";
const MAX_JSON_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_OBSERVATIONS: usize = 10_000;
const DEFAULT_MAX_NODES: usize = 256;
const DEFAULT_MAX_EDGES: usize = 512;
const DEFAULT_MAX_HOPS: usize = 32;
const ABSOLUTE_MAX_NODES: usize = 5_000;
const ABSOLUTE_MAX_EDGES: usize = 10_000;
const ABSOLUTE_MAX_HOPS: usize = 100;
const MAX_FILTER_VALUES: usize = 64;

/// Availability state for sanitized observed-flow evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedFlowState {
    /// Runtime evidence is disabled.
    #[default]
    Off,
    /// Selected runtime mode has no supported trace source.
    Unavailable,
    /// Trace snapshot is being loaded.
    Loading,
    /// Complete normalized trace is available.
    Ready,
    /// Trace is usable with visible limitations.
    Partial,
    /// Refresh failed; a previous trace may remain available.
    Failed,
    /// Workspace policy rejected access before I/O.
    Invalid,
}

/// Stable content-free observed-flow failure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedFlowIssue {
    /// Snapshot mode has no sanitized trace path.
    MissingSnapshotPath,
    /// Live runtime source exposes aggregates but no ordered trace adapter.
    UnsupportedMode,
    /// Workspace trust or filesystem policy blocks local trace access.
    WorkspaceBlocked,
    /// Snapshot path does not exist.
    NotFound,
    /// Snapshot exceeds the fixed byte bound.
    SnapshotTooLarge,
    /// Snapshot metadata or content could not be read.
    ReadFailed,
    /// Snapshot does not match the deny-by-default safe trace schema.
    InvalidData,
}

/// Safe status returned beside every observed-flow graph.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedFlowStatus {
    /// Current evidence state.
    pub state: ObservedFlowState,
    /// Stable failure class, when unavailable or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<ObservedFlowIssue>,
    /// Monotonic refresh generation.
    pub generation: u64,
    /// Most recent accepted generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success: Option<u64>,
    /// Whether a current or retained trace exists.
    pub has_snapshot: bool,
    /// Accepted normalized observation count.
    pub observations: usize,
    /// Accepted normalized session count.
    pub sessions: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ObservedFlowConfig {
    mode: RuntimeMode,
    snapshot_path: Option<PathBuf>,
}

impl ObservedFlowConfig {
    pub(crate) fn from_env(mode: RuntimeMode) -> Self {
        Self {
            mode,
            snapshot_path: std::env::var("HARNESS_LENS_TRACE_SNAPSHOT_PATH")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
        }
    }

    pub(crate) fn initial_status(&self) -> ObservedFlowStatus {
        match self.mode {
            RuntimeMode::Off => ObservedFlowStatus::default(),
            RuntimeMode::Live => ObservedFlowStatus {
                state: ObservedFlowState::Unavailable,
                issue: Some(ObservedFlowIssue::UnsupportedMode),
                ..ObservedFlowStatus::default()
            },
            RuntimeMode::Snapshot if self.snapshot_path.is_none() => ObservedFlowStatus {
                state: ObservedFlowState::Unavailable,
                issue: Some(ObservedFlowIssue::MissingSnapshotPath),
                ..ObservedFlowStatus::default()
            },
            RuntimeMode::Snapshot => ObservedFlowStatus {
                state: ObservedFlowState::Loading,
                ..ObservedFlowStatus::default()
            },
        }
    }

    pub(crate) fn can_load(&self) -> bool {
        self.mode == RuntimeMode::Snapshot && self.snapshot_path.is_some()
    }
}

pub(crate) async fn load(
    config: &ObservedFlowConfig,
) -> Result<NormalizedTrace, ObservedFlowIssue> {
    match config.mode {
        RuntimeMode::Off => return Err(ObservedFlowIssue::UnsupportedMode),
        RuntimeMode::Live => return Err(ObservedFlowIssue::UnsupportedMode),
        RuntimeMode::Snapshot => {}
    }
    let path = config
        .snapshot_path
        .as_deref()
        .ok_or(ObservedFlowIssue::MissingSnapshotPath)?;
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(classify_read_error)?;
    if metadata.len() > MAX_JSON_BYTES as u64 {
        return Err(ObservedFlowIssue::SnapshotTooLarge);
    }
    if !metadata.file_type().is_file() {
        return Err(ObservedFlowIssue::ReadFailed);
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(classify_read_error)?;
    let value = read_bounded(file).await?;
    normalize_trace_json(&value, DEFAULT_MAX_OBSERVATIONS)
        .map_err(|_| ObservedFlowIssue::InvalidData)
}

fn classify_read_error(error: std::io::Error) -> ObservedFlowIssue {
    if error.kind() == std::io::ErrorKind::NotFound {
        ObservedFlowIssue::NotFound
    } else {
        ObservedFlowIssue::ReadFailed
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<String, ObservedFlowIssue> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ObservedFlowIssue::ReadFailed)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(ObservedFlowIssue::SnapshotTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ObservedFlowIssue::InvalidData)
}

/// Width metric requested by an LSP client.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedFlowMetric {
    /// Adjacent transition count.
    #[default]
    Transitions,
    /// Distinct sessions per transition.
    DistinctSessions,
    /// Destination-action duration.
    DurationMicros,
    /// Destination-action cost in `costUnit`.
    Cost,
}

/// Parameters for one bounded observed-flow projection.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedFlowParams {
    /// Initialized file workspace root.
    pub root_uri: Uri,
    /// Optional logical action root.
    pub root: Option<String>,
    /// Maximum serialized nodes.
    pub max_nodes: Option<usize>,
    /// Maximum serialized edges.
    pub max_edges: Option<usize>,
    /// Maximum directed hops from `root`.
    pub max_hops: Option<usize>,
    /// Inclusive timestamp lower bound.
    pub window_start: Option<String>,
    /// Inclusive timestamp upper bound.
    pub window_end: Option<String>,
    /// Destination categories to retain.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Destination statuses to retain.
    #[serde(default)]
    pub statuses: Vec<RuntimeObservationStatus>,
    /// Minimum share after semantic filters.
    pub minimum_share: Option<f64>,
    /// Exactly one width metric.
    #[serde(default)]
    pub metric: ObservedFlowMetric,
    /// Required currency or billing unit for cost.
    pub cost_unit: Option<String>,
}

/// Versioned observed-flow response.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedFlowResponse {
    /// Protocol envelope version.
    pub schema_version: u32,
    /// Root owning this projection.
    pub root_uri: Uri,
    /// Safe trace refresh state.
    pub status: ObservedFlowStatus,
    /// Bounded graph with protocol-native source locations.
    pub graph: ObservedFlowGraph,
}

/// Core graph envelope with LSP-native provenance locations.
#[derive(Clone, Debug, Serialize)]
pub struct ObservedFlowGraph {
    /// Core graph schema version.
    pub schema_version: u32,
    /// Graph evidence kind.
    pub kind: GraphKind,
    /// Graph-wide method.
    pub method: ScoreMethod,
    /// Evidence availability.
    pub availability: GraphAvailability,
    /// Evidence completeness.
    pub completeness: EvidenceCompleteness,
    /// Applied bounds.
    pub limits: GraphLimits,
    /// Active filters and metric unit.
    pub filters: GraphFilters,
    /// Layered nodes.
    pub nodes: Vec<ObservedFlowNode>,
    /// Weighted directed transitions.
    pub edges: Vec<ObservedFlowEdge>,
}

/// Protocol node with optional UTF-16 navigation provenance.
#[derive(Clone, Debug, Serialize)]
pub struct ObservedFlowNode {
    /// Stable serialized node identity.
    pub id: String,
    /// Canonical identity shared across layers.
    pub logical_id: String,
    /// Safe display label.
    pub label: String,
    /// Semantic node kind.
    pub kind: GraphNodeKind,
    /// Sequence layer.
    pub layer: Option<usize>,
    /// Bounded evidence references.
    pub provenance: Vec<ObservedFlowProvenance>,
}

/// Protocol edge with optional UTF-16 navigation provenance.
#[derive(Clone, Debug, Serialize)]
pub struct ObservedFlowEdge {
    /// Stable edge identity.
    pub id: String,
    /// Source node identity.
    pub source: String,
    /// Target node identity.
    pub target: String,
    /// Observed transition semantics.
    pub relationship: GraphRelationship,
    /// Statistical aggregation method.
    pub method: ScoreMethod,
    /// Required declared width metric.
    pub metric: Option<WeightedEdgeMetric>,
    /// Bounded evidence references.
    pub provenance: Vec<ObservedFlowProvenance>,
}

/// Protocol provenance with LSP source location.
#[derive(Clone, Debug, Serialize)]
pub struct ObservedFlowProvenance {
    /// Stable adapter source.
    pub source: String,
    /// Evidence method.
    pub method: ScoreMethod,
    /// Safe bounded evidence identities.
    pub evidence_ids: Vec<String>,
    /// Evidence count before the reference bound.
    pub total_evidence: usize,
    /// Safe UTF-16 source location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

pub(crate) fn build_response(
    root_uri: Uri,
    root: &Path,
    open_documents: &BTreeMap<PathBuf, String>,
    trace: Option<&NormalizedTrace>,
    status: ObservedFlowStatus,
    params: &ObservedFlowParams,
) -> Result<ObservedFlowResponse, String> {
    let options = options(params)?;
    let mut graph = match trace {
        Some(trace) => build_observed_flow(&trace.trace, &options)
            .map_err(|error| format!("cannot build observed flow: {error}"))?,
        None => {
            let graph = unavailable_graph(&options, status.issue);
            graph
                .validate()
                .map_err(|error| format!("invalid unavailable observed flow: {error}"))?;
            graph
        }
    };
    if status.state == ObservedFlowState::Failed && status.has_snapshot {
        graph.completeness.complete = false;
        graph.completeness.reasons.push(CompletenessReason {
            code: "stale_snapshot".to_owned(),
            count: None,
        });
        graph = graph.canonicalize();
        graph
            .validate()
            .map_err(|error| format!("invalid retained observed flow: {error}"))?;
    }
    Ok(ObservedFlowResponse {
        schema_version: 1,
        root_uri,
        status,
        graph: protocol_graph(root, open_documents, graph),
    })
}

fn options(params: &ObservedFlowParams) -> Result<ObservedFlowOptions, String> {
    let max_nodes = params.max_nodes.unwrap_or(DEFAULT_MAX_NODES);
    let max_edges = params.max_edges.unwrap_or(DEFAULT_MAX_EDGES);
    let max_hops = params.max_hops.unwrap_or(DEFAULT_MAX_HOPS);
    if max_nodes == 0 || max_nodes > ABSOLUTE_MAX_NODES {
        return Err(format!(
            "maxNodes must be between 1 and {ABSOLUTE_MAX_NODES}"
        ));
    }
    if max_edges == 0 || max_edges > ABSOLUTE_MAX_EDGES {
        return Err(format!(
            "maxEdges must be between 1 and {ABSOLUTE_MAX_EDGES}"
        ));
    }
    if max_hops == 0 || max_hops > ABSOLUTE_MAX_HOPS {
        return Err(format!("maxHops must be between 1 and {ABSOLUTE_MAX_HOPS}"));
    }
    if params.categories.len() > MAX_FILTER_VALUES || params.statuses.len() > MAX_FILTER_VALUES {
        return Err(format!(
            "category and status filters cannot exceed {MAX_FILTER_VALUES} values"
        ));
    }
    let window = match (&params.window_start, &params.window_end) {
        (None, None) => None,
        (Some(start), Some(end)) => Some(ObservationWindow {
            start: start.clone(),
            end: end.clone(),
        }),
        _ => return Err("windowStart and windowEnd must be supplied together".to_owned()),
    };
    let metric = match params.metric {
        ObservedFlowMetric::Transitions => FlowMetric::Transitions,
        ObservedFlowMetric::DistinctSessions => FlowMetric::DistinctSessions,
        ObservedFlowMetric::DurationMicros => FlowMetric::DurationMicros,
        ObservedFlowMetric::Cost => FlowMetric::Cost(
            params
                .cost_unit
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "costUnit is required for cost metric".to_owned())?,
        ),
    };
    if params.metric != ObservedFlowMetric::Cost && params.cost_unit.is_some() {
        return Err("costUnit is valid only for cost metric".to_owned());
    }
    Ok(ObservedFlowOptions {
        root: params.root.clone(),
        max_nodes,
        max_edges,
        max_hops,
        window,
        categories: params.categories.iter().cloned().collect(),
        statuses: params.statuses.iter().copied().collect(),
        minimum_share: params.minimum_share,
        metric,
    })
}

fn unavailable_graph(
    options: &ObservedFlowOptions,
    issue: Option<ObservedFlowIssue>,
) -> RelationshipGraph {
    RelationshipGraph {
        schema_version: RELATIONSHIP_GRAPH_SCHEMA_VERSION,
        kind: GraphKind::ObservedFlow,
        method: ScoreMethod::Statistical,
        availability: GraphAvailability::Unavailable,
        completeness: EvidenceCompleteness {
            complete: false,
            reasons: vec![CompletenessReason {
                code: match issue {
                    Some(ObservedFlowIssue::WorkspaceBlocked) => "workspace_blocked",
                    Some(ObservedFlowIssue::InvalidData) => "invalid_data",
                    Some(ObservedFlowIssue::SnapshotTooLarge) => "snapshot_too_large",
                    Some(ObservedFlowIssue::ReadFailed) => "read_failed",
                    Some(ObservedFlowIssue::NotFound) => "not_found",
                    Some(ObservedFlowIssue::MissingSnapshotPath) => "missing_snapshot_path",
                    Some(ObservedFlowIssue::UnsupportedMode) => "unsupported_mode",
                    None => "runtime_off",
                }
                .to_owned(),
                count: None,
            }],
        },
        limits: GraphLimits {
            max_nodes: options.max_nodes,
            max_edges: options.max_edges,
            max_hops: options.max_hops,
        },
        filters: GraphFilters {
            root: options.root.clone(),
            window: options.window.clone(),
            categories: options.categories.iter().cloned().collect(),
            statuses: options.statuses.iter().copied().collect(),
            minimum_share: options.minimum_share,
            metric_unit: options.metric.unit().to_owned(),
        },
        nodes: Vec::new(),
        edges: Vec::new(),
    }
    .canonicalize()
}

fn protocol_graph(
    root: &Path,
    open_documents: &BTreeMap<PathBuf, String>,
    graph: RelationshipGraph,
) -> ObservedFlowGraph {
    ObservedFlowGraph {
        schema_version: graph.schema_version,
        kind: graph.kind,
        method: graph.method,
        availability: graph.availability,
        completeness: graph.completeness,
        limits: graph.limits,
        filters: graph.filters,
        nodes: graph
            .nodes
            .into_iter()
            .map(|node| ObservedFlowNode {
                id: node.id,
                logical_id: node.logical_id,
                label: node.label,
                kind: node.kind,
                layer: node.layer,
                provenance: node
                    .provenance
                    .into_iter()
                    .map(|value| provenance(root, open_documents, value))
                    .collect(),
            })
            .collect(),
        edges: graph
            .edges
            .into_iter()
            .map(|edge| ObservedFlowEdge {
                id: edge.id,
                source: edge.source,
                target: edge.target,
                relationship: edge.relationship,
                method: edge.method,
                metric: edge.metric,
                provenance: edge
                    .provenance
                    .into_iter()
                    .map(|value| provenance(root, open_documents, value))
                    .collect(),
            })
            .collect(),
    }
}

fn provenance(
    root: &Path,
    open_documents: &BTreeMap<PathBuf, String>,
    value: harness_lens::GraphProvenance,
) -> ObservedFlowProvenance {
    ObservedFlowProvenance {
        source: value.source,
        method: value.method,
        evidence_ids: value.evidence_ids,
        total_evidence: value.total_evidence,
        location: value
            .location
            .and_then(|location| source_location(root, open_documents, &location)),
    }
}

fn source_location(
    root: &Path,
    open_documents: &BTreeMap<PathBuf, String>,
    location: &EvidenceLocation,
) -> Option<Location> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = root.join(&location.path);
    let candidate = candidate.canonicalize().unwrap_or(candidate);
    if !candidate.starts_with(&root) {
        return None;
    }
    let uri = file_uri(&candidate)?;
    let content = open_documents
        .get(&candidate)
        .cloned()
        .or_else(|| std::fs::read_to_string(&candidate).ok());
    let range = content
        .as_deref()
        .and_then(|content| {
            location
                .span
                .and_then(|span| range_from_byte_span(content, span))
                .or_else(|| location.line.map(|line| whole_line_range(content, line)))
        })
        .unwrap_or_default();
    Some(Location::new(uri, range))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "harness-lens-flow-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn params(root_uri: Uri) -> ObservedFlowParams {
        ObservedFlowParams {
            root_uri,
            root: None,
            max_nodes: None,
            max_edges: None,
            max_hops: None,
            window_start: None,
            window_end: None,
            categories: Vec::new(),
            statuses: Vec::new(),
            minimum_share: None,
            metric: ObservedFlowMetric::Transitions,
            cost_unit: None,
        }
    }

    #[tokio::test]
    async fn loads_bounded_deny_by_default_trace_snapshot() {
        let root = temp_root("load");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("trace.json");
        fs::write(
            &path,
            r#"{"schema_version":1,"window":{"start":"a","end":"z"},"complete":true,"observations":[]}"#,
        )
        .unwrap();
        let config = ObservedFlowConfig {
            mode: RuntimeMode::Snapshot,
            snapshot_path: Some(path.clone()),
        };
        assert!(load(&config).await.is_ok());

        fs::write(
            &path,
            r#"{"schema_version":1,"window":{"start":"a","end":"z"},"complete":true,"prompt":"secret","observations":[]}"#,
        )
        .unwrap();
        assert_eq!(load(&config).await, Err(ObservedFlowIssue::InvalidData));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unavailable_response_keeps_filters_and_named_unit() {
        let root = temp_root("empty");
        let root_uri = Uri::from_file_path(&root).unwrap();
        let mut request = params(root_uri.clone());
        request.categories.push("tool".to_owned());
        request.minimum_share = Some(0.1);
        let response = build_response(
            root_uri,
            &root,
            &BTreeMap::new(),
            None,
            ObservedFlowStatus::default(),
            &request,
        )
        .unwrap();
        assert_eq!(response.graph.availability, GraphAvailability::Unavailable);
        assert_eq!(response.graph.filters.metric_unit, "transitions");
        assert_eq!(response.graph.filters.minimum_share, Some(0.1));
    }

    #[test]
    fn ready_response_maps_bounded_provenance_to_utf16() {
        let root = temp_root("ready");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("action.md");
        fs::write(&source, "😀go\n").unwrap();
        let trace = normalize_trace_json(
            r#"{
              "schema_version":1,
              "window":{"start":"a","end":"z"},
              "complete":true,
              "observations":[
                {"id":"one","session_id":"s","sequence":1,
                 "action":{"id":"read","label":"Read","category":"tool"},
                 "status":"success"},
                {"id":"two","session_id":"s","sequence":2,
                 "action":{"id":"write","label":"Write","category":"tool"},
                 "status":"success",
                 "location":{"path":"action.md","span":{"start":4,"end":6}}}
              ]
            }"#,
            10,
        )
        .unwrap();
        let root_uri = Uri::from_file_path(&root).unwrap();
        let response = build_response(
            root_uri.clone(),
            &root,
            &BTreeMap::new(),
            Some(&trace),
            ObservedFlowStatus {
                state: ObservedFlowState::Ready,
                generation: 1,
                last_success: Some(1),
                has_snapshot: true,
                observations: 2,
                sessions: 1,
                ..ObservedFlowStatus::default()
            },
            &params(root_uri),
        )
        .unwrap();
        assert_eq!(response.graph.availability, GraphAvailability::Ready);
        assert_eq!(response.graph.nodes.len(), 2);
        assert_eq!(response.graph.edges.len(), 1);
        let location = response.graph.edges[0].provenance[0]
            .location
            .as_ref()
            .unwrap();
        assert_eq!(location.range.start.character, 2);
        assert_eq!(location.range.end.character, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_bounds_windows_and_cost_units() {
        let root = temp_root("params");
        let mut request = params(Uri::from_file_path(root).unwrap());
        request.max_nodes = Some(0);
        assert!(options(&request).unwrap_err().contains("maxNodes"));
        request.max_nodes = None;
        request.window_start = Some("a".to_owned());
        assert!(options(&request).unwrap_err().contains("supplied together"));
        request.window_start = None;
        request.metric = ObservedFlowMetric::Cost;
        assert!(options(&request).unwrap_err().contains("costUnit"));
    }
}
