// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

//! Bounded provider catalog and aggregate protocol mapping.

use std::collections::BTreeSet;
use std::path::PathBuf;

use harness_lens::providers::{
    AggregateEnvelope, CODEBURN_ID, NATIVE_ID, ProviderAssumption, ProviderAttempt,
    ProviderAvailability, ProviderContext, ProviderContribution, ProviderError, ProviderHealth,
    ProviderMetric, ProviderReport, ProviderService, ProviderStatus, RefreshState, merge_reports,
};
use harness_lens::{AnalysisReport, RuntimeMode as CoreRuntimeMode, ScoreMethod};
use harness_metrics::CodeBurnSnapshot;
use serde::{Deserialize, Serialize};
use tower_lsp_server::ls_types::Uri;

use crate::runtime_metrics::{
    RuntimeConfig, RuntimeIssue, RuntimeMode, RuntimeState, RuntimeStatus,
};

pub(crate) const CATALOG_METHOD: &str = "harnessLens/providerCatalog";
pub(crate) const AGGREGATE_METHOD: &str = "harnessLens/providerAggregate";

/// Empty parameters reserved for forward-compatible catalog filtering.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProviderCatalogParams {}

/// Versioned provider catalog response.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogResponse {
    /// Provider protocol version.
    pub schema_version: u32,
    /// Deterministically ordered local provider status.
    pub providers: Vec<ProviderStatus>,
    /// Safe optional-selection error that did not disable Native.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<ProviderError>,
}

/// Parameters for one root-bounded provider aggregate.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAggregateParams {
    /// Initialized file workspace root.
    pub root_uri: Uri,
    /// Maximum discovered files; defaults to the workspace report bound.
    pub max_files: Option<usize>,
}

/// Native report plus namespaced optional-provider contributions for one root.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAggregateResponse {
    /// Provider protocol version.
    pub schema_version: u32,
    /// Root owning this report.
    pub root_uri: Uri,
    /// Unmodified Native report. Provider contributions cannot change it.
    pub native: AnalysisReport,
    /// Namespaced provider reports, provenance, and safe refresh state.
    pub aggregate: AggregateEnvelope,
    /// Existing safe runtime status; raw provider output is never included.
    pub runtime: RuntimeStatus,
    /// Safe optional-selection error that did not disable Native.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<ProviderError>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderHostConfig {
    pub(crate) selected: BTreeSet<String>,
    pub(crate) workspace_trusted: bool,
    pub(crate) virtual_workspace: bool,
    pub(crate) issue: Option<ProviderError>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializationEnvelope {
    harness_lens: Option<ProviderInitialization>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInitialization {
    workspace_trusted: Option<bool>,
    virtual_workspace: Option<bool>,
    selected_providers: Option<Vec<String>>,
}

impl ProviderHostConfig {
    pub(crate) fn from_initialization(
        value: Option<&serde_json::Value>,
        runtime_mode: RuntimeMode,
    ) -> Self {
        let parsed = value
            .cloned()
            .map(serde_json::from_value::<InitializationEnvelope>)
            .transpose();
        let mut config = Self {
            workspace_trusted: bool_env("HARNESS_LENS_WORKSPACE_TRUSTED"),
            virtual_workspace: bool_env("HARNESS_LENS_VIRTUAL_WORKSPACE"),
            ..Self::default()
        };
        let initialization = match parsed {
            Ok(value) => value.and_then(|value| value.harness_lens),
            Err(_) => {
                config.issue = Some(ProviderError::ConfigurationError);
                None
            }
        };
        if let Some(value) = initialization {
            config.workspace_trusted = value.workspace_trusted.unwrap_or(config.workspace_trusted);
            config.virtual_workspace = value.virtual_workspace.unwrap_or(config.virtual_workspace);
            if let Some(selected) = value.selected_providers {
                config.selected = selected.into_iter().collect();
            } else if runtime_mode != RuntimeMode::Off {
                config.selected.insert(CODEBURN_ID.to_owned());
            }
        } else if runtime_mode != RuntimeMode::Off {
            config.selected.insert(CODEBURN_ID.to_owned());
        }
        let allowed = [NATIVE_ID, CODEBURN_ID]
            .into_iter()
            .collect::<BTreeSet<_>>();
        if config
            .selected
            .iter()
            .any(|provider| !allowed.contains(provider.as_str()))
        {
            config.selected.clear();
            config.issue = Some(ProviderError::InvalidProvider);
        }
        config
    }

    pub(crate) fn context(&self, runtime: &RuntimeConfig) -> ProviderContext {
        ProviderContext {
            selected: if self.issue.is_some() {
                BTreeSet::new()
            } else {
                self.selected.clone()
            },
            runtime_mode: core_runtime_mode(runtime.mode),
            snapshot_configured: runtime.snapshot_path.is_some(),
            workspace_trusted: self.workspace_trusted,
            virtual_workspace: self.virtual_workspace,
            codeburn_executable: PathBuf::from(&runtime.executable),
        }
    }

    pub(crate) fn runtime_blocker(&self) -> Option<RuntimeIssue> {
        if let Some(issue) = self.issue {
            return Some(if issue == ProviderError::InvalidProvider {
                RuntimeIssue::InvalidProvider
            } else {
                RuntimeIssue::InvalidProviderConfiguration
            });
        }
        if !self.selected.contains(CODEBURN_ID) {
            return None;
        }
        if !self.workspace_trusted || self.virtual_workspace {
            return Some(RuntimeIssue::WorkspaceBlocked);
        }
        None
    }
}

pub(crate) fn build_catalog(
    host: &ProviderHostConfig,
    runtime_config: &RuntimeConfig,
    runtime_status: &RuntimeStatus,
    generation: u64,
    last_success: Option<u64>,
) -> Result<ProviderCatalogResponse, ProviderError> {
    let context = host.context(runtime_config);
    let mut providers = ProviderService::default().catalog(&context)?;
    if let Some(status) = providers
        .iter_mut()
        .find(|status| status.descriptor.id == CODEBURN_ID)
    {
        apply_runtime_status(status, runtime_status, generation, last_success);
    }
    Ok(ProviderCatalogResponse {
        schema_version: 1,
        providers,
        issue: host.issue,
    })
}

pub(crate) fn build_aggregate(
    native: AnalysisReport,
    snapshot: Option<&CodeBurnSnapshot>,
    runtime_status: &RuntimeStatus,
    host: &ProviderHostConfig,
    native_generation: u64,
    runtime_generation: u64,
    runtime_last_success: Option<u64>,
) -> Result<(AnalysisReport, AggregateEnvelope), ProviderError> {
    let allowed = [NATIVE_ID.to_owned(), CODEBURN_ID.to_owned()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let selected = host.selected.clone();
    let native_attempt = ProviderAttempt {
        provider_id: NATIVE_ID.to_owned(),
        generation: native_generation,
        result: Ok(native_report(&native, native_generation)),
    };
    let mut aggregate = merge_reports(&allowed, &selected, vec![native_attempt], None, false)?;
    if !selected.contains(CODEBURN_ID) {
        return Ok((native, aggregate));
    }
    if let Some(blocker) = host.runtime_blocker() {
        aggregate = merge_reports(
            &allowed,
            &selected,
            vec![failed_attempt(runtime_generation, provider_error(blocker))],
            Some(&aggregate),
            true,
        )?;
        return Ok((native, aggregate));
    }
    if runtime_status.mode == RuntimeMode::Off {
        return Ok((native, aggregate));
    }

    if let Some(snapshot) = snapshot {
        let accepted_generation = runtime_last_success.unwrap_or(runtime_generation);
        let report = codeburn_report(snapshot, accepted_generation)?;
        aggregate = merge_reports(
            &allowed,
            &selected,
            vec![ProviderAttempt {
                provider_id: CODEBURN_ID.to_owned(),
                generation: accepted_generation,
                result: Ok(report),
            }],
            Some(&aggregate),
            true,
        )?;
        if runtime_status.state == RuntimeState::Failed {
            aggregate = merge_reports(
                &allowed,
                &selected,
                vec![failed_attempt(
                    runtime_generation,
                    provider_error(runtime_status.issue.unwrap_or(RuntimeIssue::Unavailable)),
                )],
                Some(&aggregate),
                true,
            )?;
        }
    } else if matches!(
        runtime_status.state,
        RuntimeState::Failed | RuntimeState::Invalid
    ) {
        aggregate = merge_reports(
            &allowed,
            &selected,
            vec![failed_attempt(
                runtime_generation,
                provider_error(runtime_status.issue.unwrap_or(RuntimeIssue::Unavailable)),
            )],
            Some(&aggregate),
            true,
        )?;
    }
    Ok((native, aggregate))
}

fn native_report(report: &AnalysisReport, generation: u64) -> ProviderReport {
    ProviderReport {
        provider_id: NATIVE_ID.to_owned(),
        generation,
        contributions: vec![ProviderContribution {
            metric: ProviderMetric::Sources,
            value: report.sources.len() as f64,
            method: ScoreMethod::Deterministic,
            sample_size: None,
            assumptions: vec![ProviderAssumption::LocalInventory],
            evidence: Vec::new(),
            fingerprint: None,
        }],
    }
}

fn codeburn_report(
    snapshot: &CodeBurnSnapshot,
    generation: u64,
) -> Result<ProviderReport, ProviderError> {
    let overview = &snapshot.report.overview;
    let tokens = overview
        .tokens
        .input
        .checked_add(overview.tokens.output)
        .and_then(|value| value.checked_add(overview.tokens.cache_read))
        .and_then(|value| value.checked_add(overview.tokens.cache_write))
        .ok_or(ProviderError::LimitExceeded)?;
    let assumptions = vec![
        ProviderAssumption::RuntimeAggregateNotCausal,
        ProviderAssumption::ProviderWindow,
    ];
    let mut contributions = vec![
        statistical(
            ProviderMetric::Calls,
            overview.calls,
            overview.calls,
            &assumptions,
        ),
        statistical(
            ProviderMetric::Sessions,
            overview.sessions,
            overview.sessions,
            &assumptions,
        ),
        statistical(ProviderMetric::Tokens, tokens, overview.calls, &assumptions),
    ];
    if snapshot.report.currency.eq_ignore_ascii_case("USD") && overview.estimated_cost == 0.0 {
        if !overview.cost.is_finite() || overview.cost < 0.0 {
            return Err(ProviderError::InvalidReport);
        }
        contributions.push(ProviderContribution {
            metric: ProviderMetric::CostUsd,
            value: overview.cost,
            method: ScoreMethod::Statistical,
            sample_size: Some(overview.calls),
            assumptions,
            evidence: Vec::new(),
            fingerprint: None,
        });
    }
    Ok(ProviderReport {
        provider_id: CODEBURN_ID.to_owned(),
        generation,
        contributions,
    })
}

fn statistical(
    metric: ProviderMetric,
    value: u64,
    sample_size: u64,
    assumptions: &[ProviderAssumption],
) -> ProviderContribution {
    ProviderContribution {
        metric,
        value: value as f64,
        method: ScoreMethod::Statistical,
        sample_size: Some(sample_size),
        assumptions: assumptions.to_vec(),
        evidence: Vec::new(),
        fingerprint: None,
    }
}

fn failed_attempt(generation: u64, error: ProviderError) -> ProviderAttempt {
    ProviderAttempt {
        provider_id: CODEBURN_ID.to_owned(),
        generation,
        result: Err(error),
    }
}

fn apply_runtime_status(
    status: &mut ProviderStatus,
    runtime: &RuntimeStatus,
    generation: u64,
    last_success: Option<u64>,
) {
    status.refresh = RefreshState {
        generation,
        last_success,
        health: match runtime.state {
            RuntimeState::Off => ProviderHealth::Disabled,
            RuntimeState::Loading => ProviderHealth::NeverRefreshed,
            RuntimeState::Ready => ProviderHealth::Healthy,
            RuntimeState::Failed if runtime.has_snapshot => ProviderHealth::Stale,
            RuntimeState::Invalid if runtime.issue == Some(RuntimeIssue::WorkspaceBlocked) => {
                ProviderHealth::Disabled
            }
            RuntimeState::Failed | RuntimeState::Invalid => ProviderHealth::Failed,
        },
        error: runtime.issue.map(provider_error),
    };
    if matches!(runtime.state, RuntimeState::Failed | RuntimeState::Invalid) {
        status.availability = match status.refresh.error {
            Some(ProviderError::NotFound) => ProviderAvailability::NotFound,
            Some(ProviderError::InvalidVersion) => ProviderAvailability::InvalidVersion,
            Some(ProviderError::ConfigurationError) => ProviderAvailability::ConfigurationError,
            Some(ProviderError::WorkspaceBlocked) => ProviderAvailability::Blocked,
            _ => ProviderAvailability::RuntimeFailure,
        };
    }
}

pub(crate) fn provider_error(issue: RuntimeIssue) -> ProviderError {
    match issue {
        RuntimeIssue::InvalidMode
        | RuntimeIssue::InvalidPeriod
        | RuntimeIssue::MissingSnapshotPath
        | RuntimeIssue::InvalidProvider
        | RuntimeIssue::InvalidProviderConfiguration => ProviderError::ConfigurationError,
        RuntimeIssue::WorkspaceBlocked => ProviderError::WorkspaceBlocked,
        RuntimeIssue::SnapshotTooLarge => ProviderError::LimitExceeded,
        RuntimeIssue::NotFound => ProviderError::NotFound,
        RuntimeIssue::Unavailable => ProviderError::RuntimeFailure,
        RuntimeIssue::Timeout => ProviderError::Timeout,
        RuntimeIssue::CommandFailed | RuntimeIssue::ReadFailed => ProviderError::RuntimeFailure,
        RuntimeIssue::InvalidData => ProviderError::InvalidReport,
    }
}

fn core_runtime_mode(mode: RuntimeMode) -> CoreRuntimeMode {
    match mode {
        RuntimeMode::Off => CoreRuntimeMode::Off,
        RuntimeMode::Live => CoreRuntimeMode::Live,
        RuntimeMode::Snapshot => CoreRuntimeMode::Snapshot,
    }
}

fn bool_env(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_lens::{HarnessLensConfig, Scanner};
    use harness_metrics::{Overview, TokenTotals};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn native() -> AnalysisReport {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harness-lens-provider-protocol-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temporary workspace");
        let report = Scanner::new()
            .scan(&root, &HarnessLensConfig::default())
            .expect("scan temporary workspace");
        std::fs::remove_dir_all(root).expect("remove temporary workspace");
        report
    }

    fn runtime(mode: RuntimeMode, state: RuntimeState) -> RuntimeStatus {
        RuntimeStatus {
            mode,
            state,
            period: "30days".to_owned(),
            ..RuntimeStatus::default()
        }
    }

    #[test]
    fn initialization_defaults_to_native_only_and_rejects_unknown_ids() {
        let safe = serde_json::json!({
            "harnessLens": {"workspaceTrusted": false, "virtualWorkspace": false}
        });
        let off = ProviderHostConfig::from_initialization(Some(&safe), RuntimeMode::Off);
        assert!(off.selected.is_empty());
        assert!(!off.workspace_trusted);

        let value = serde_json::json!({
            "harnessLens": {
                "workspaceTrusted": true,
                "selectedProviders": ["remote"]
            }
        });
        let invalid = ProviderHostConfig::from_initialization(Some(&value), RuntimeMode::Live);
        assert!(invalid.selected.is_empty());
        assert_eq!(invalid.issue, Some(ProviderError::InvalidProvider));
    }

    #[test]
    fn malformed_initialization_falls_back_without_optional_process() {
        let value = serde_json::json!({"harnessLens": "invalid"});
        let host = ProviderHostConfig::from_initialization(Some(&value), RuntimeMode::Live);
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            executable: "must-not-run".to_owned(),
            period: "30days".to_owned(),
            snapshot_path: None,
            issue: None,
        };

        let response = build_catalog(
            &host,
            &config,
            &runtime(RuntimeMode::Live, RuntimeState::Invalid),
            0,
            None,
        )
        .unwrap();

        assert_eq!(response.issue, Some(ProviderError::ConfigurationError));
        let codeburn = response
            .providers
            .iter()
            .find(|status| status.descriptor.id == CODEBURN_ID)
            .unwrap();
        assert!(!codeburn.selected);
    }

    #[test]
    fn live_mode_requires_explicit_workspace_trust() {
        let blocked = serde_json::json!({"harnessLens": {"workspaceTrusted": false}});
        let host = ProviderHostConfig::from_initialization(Some(&blocked), RuntimeMode::Live);
        assert!(host.selected.contains(CODEBURN_ID));
        assert_eq!(host.runtime_blocker(), Some(RuntimeIssue::WorkspaceBlocked));

        let value = serde_json::json!({"harnessLens": {"workspaceTrusted": true}});
        let trusted = ProviderHostConfig::from_initialization(Some(&value), RuntimeMode::Live);
        assert_eq!(trusted.runtime_blocker(), None);
    }

    #[test]
    fn native_report_stays_separate_from_provider_metrics() {
        let host = ProviderHostConfig::default();
        let expected = native();
        let (native, aggregate) = build_aggregate(
            expected.clone(),
            None,
            &runtime(RuntimeMode::Off, RuntimeState::Off),
            &host,
            0,
            0,
            None,
        )
        .unwrap();
        assert_eq!(native, expected);
        assert_eq!(aggregate.selected_providers, [NATIVE_ID]);
        assert!(aggregate.reports.contains_key(NATIVE_ID));
        assert!(!aggregate.reports.contains_key(CODEBURN_ID));
    }

    #[test]
    fn codeburn_metrics_are_namespaced_statistical_and_sample_bearing() {
        let mut host = ProviderHostConfig {
            workspace_trusted: true,
            ..ProviderHostConfig::default()
        };
        host.selected.insert(CODEBURN_ID.to_owned());
        let snapshot = CodeBurnSnapshot {
            report: harness_metrics::CodeBurnReport {
                currency: "USD".to_owned(),
                overview: Overview {
                    cost: 2.5,
                    calls: 12,
                    sessions: 3,
                    tokens: TokenTotals {
                        input: 10,
                        output: 20,
                        cache_read: 30,
                        cache_write: 40,
                    },
                    ..Overview::default()
                },
                ..harness_metrics::CodeBurnReport::default()
            },
            ..CodeBurnSnapshot::default()
        };
        let (_, aggregate) = build_aggregate(
            native(),
            Some(&snapshot),
            &runtime(RuntimeMode::Live, RuntimeState::Ready),
            &host,
            1,
            1,
            Some(1),
        )
        .unwrap();
        let report = &aggregate.reports[CODEBURN_ID];
        assert_eq!(report.contributions.len(), 4);
        assert!(report.contributions.iter().all(|contribution| {
            contribution.method == ScoreMethod::Statistical
                && contribution.sample_size.is_some()
                && contribution
                    .assumptions
                    .contains(&ProviderAssumption::RuntimeAggregateNotCausal)
        }));
        assert_eq!(
            report
                .contributions
                .iter()
                .find(|contribution| contribution.metric == ProviderMetric::Tokens)
                .unwrap()
                .value,
            100.0
        );

        let mut estimated = snapshot;
        estimated.report.overview.estimated_cost = 1.0;
        let (_, aggregate) = build_aggregate(
            native(),
            Some(&estimated),
            &runtime(RuntimeMode::Live, RuntimeState::Ready),
            &host,
            2,
            2,
            Some(2),
        )
        .unwrap();
        assert!(
            aggregate.reports[CODEBURN_ID]
                .contributions
                .iter()
                .all(|contribution| contribution.metric != ProviderMetric::CostUsd)
        );
    }

    #[test]
    fn failed_refresh_retains_valid_snapshot_as_stale() {
        let mut host = ProviderHostConfig {
            workspace_trusted: true,
            ..ProviderHostConfig::default()
        };
        host.selected.insert(CODEBURN_ID.to_owned());
        let mut failed = runtime(RuntimeMode::Live, RuntimeState::Failed);
        failed.issue = Some(RuntimeIssue::Timeout);
        failed.has_snapshot = true;
        let (_, aggregate) = build_aggregate(
            native(),
            Some(&CodeBurnSnapshot::default()),
            &failed,
            &host,
            2,
            2,
            Some(1),
        )
        .unwrap();
        assert!(aggregate.reports.contains_key(CODEBURN_ID));
        assert_eq!(aggregate.refresh[CODEBURN_ID].health, ProviderHealth::Stale);
        assert_eq!(
            aggregate.refresh[CODEBURN_ID].error,
            Some(ProviderError::Timeout)
        );
    }

    #[test]
    fn blocked_workspace_clears_optional_snapshot() {
        let mut host = ProviderHostConfig::default();
        host.selected.insert(CODEBURN_ID.to_owned());
        let (_, aggregate) = build_aggregate(
            native(),
            Some(&CodeBurnSnapshot::default()),
            &runtime(RuntimeMode::Live, RuntimeState::Ready),
            &host,
            2,
            2,
            Some(1),
        )
        .unwrap();
        assert!(!aggregate.reports.contains_key(CODEBURN_ID));
        assert_eq!(
            aggregate.refresh[CODEBURN_ID].error,
            Some(ProviderError::WorkspaceBlocked)
        );
    }
}
