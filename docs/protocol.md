> SPDX-License-Identifier: MPL-2.0
> Copyright © 2026 Cristian Camargo Filho

# Protocol scope

Reference Rust capabilities:

- full-document synchronization for unsaved-buffer overlays;
- repository discovery and diagnostics for recognized harness files;
- cross-file analysis within the deepest matching workspace root;
- stable rule codes and evidence ranges;
- warning/error severity mapping.
- bounded `harnessLens/workspaceReport` transport;
- bounded `harnessLens/providerCatalog` and `harnessLens/providerAggregate`
  transport;
- opt-in CodeBurn aggregate status, hover, code lenses, and `HMxxx` diagnostics.

The TypeScript compatibility server currently provides incremental
single-document validation.

Planned capabilities:

- profile configuration;
- quick fixes and rule explanations;
- snapshot and trend requests.

## Workspace report request

`harnessLens/workspaceReport` exposes the content-safe deterministic report
already consumed by CLI and diagnostics. It never serializes source contents.

Request parameters:

```json
{
  "rootUri": "file:///workspace",
  "maxFiles": 5000
}
```

`rootUri` is optional. When omitted, the response contains one report per
initialized workspace root. Open editor documents overlay filesystem content
without mutating history. `maxFiles` defaults to 5,000, must be between 1 and
50,000, and lowers any looser repository discovery limit. Reaching the bound is
reported as incomplete coverage.

Response envelope:

```json
{
  "schemaVersion": 1,
  "reports": [],
  "runtime": {
    "mode": "off",
    "state": "off",
    "period": "30days",
    "calls": 0,
    "sessions": 0,
    "warningCount": 0,
    "hasSnapshot": false
  }
}
```

Each report retains Core's schema version, completeness reasons, content-free
source records, findings, per-file and aggregate metrics, normalized scores,
and observable plugin executions. Source spans remain UTF-8 byte ranges in the
report; only diagnostic adapters convert positions to UTF-16.

## Provider initialization

Native is always selected. Optional provider access also requires explicit
workspace policy from the client:

```json
{
  "initializationOptions": {
    "harnessLens": {
      "workspaceTrusted": true,
      "virtualWorkspace": false,
      "selectedProviders": ["codeburn"]
    }
  }
}
```

Unknown IDs disable optional providers and remain visible as a safe
`invalid_provider` issue; Native analysis continues. Without initialization
options, `HARNESS_LENS_WORKSPACE_TRUSTED=true` and
`HARNESS_LENS_VIRTUAL_WORKSPACE=false` provide equivalent host policy. Invalid
or absent trust never enables optional execution. A non-off runtime mode selects
CodeBurn by default unless `selectedProviders` is supplied explicitly.

## Provider catalog request

`harnessLens/providerCatalog` accepts `{}`. It returns schema version 1 and the
compiled-in provider catalog in stable ID order. Status separates selection,
availability, installation, refresh generation, health, and safe error class.
Native reports MPL-2.0 built-in status. CodeBurn reports MIT optional status.

Live catalog detection runs the SDK's bounded `codeburn --version` adapter only
when CodeBurn is selected and workspace policy allows it. Off and snapshot
catalog requests launch no process. Provider stdout, stderr, arguments, paths,
source, and credentials are absent from the response.

## Provider aggregate request

`harnessLens/providerAggregate` returns one root-bounded Native report beside a
Core `AggregateEnvelope`:

```json
{
  "rootUri": "file:///workspace",
  "maxFiles": 5000
}
```

`rootUri` must name an initialized file workspace. `maxFiles` has the same
1–50,000 bounds as `harnessLens/workspaceReport`. Native source count remains in
the `harness-lens-native` namespace. Available CodeBurn calls, sessions, tokens,
and non-estimated USD cost remain in the `codeburn` namespace. Estimated or
non-USD cost is omitted because Core's fixed contribution contract cannot label
its currency/basis safely. Runtime measurements are
statistical, expose sample size, and declare runtime-window/non-causality
assumptions. Values are raw measurements and never enter Native scores.

Refresh generations follow request order. A failed optional refresh retains a
previous validated report as `stale`; trust, virtual-workspace, configuration,
mode, or selection changes clear affected optional state. Payload bounds and
Core provider/contribution bounds apply before serialization. The response also
contains safe runtime status and never contains Harness source content or raw
CodeBurn JSON.

## Runtime evidence

`off` is default and performs no runtime I/O. Trusted, selected `live` mode runs
CodeBurn `report`, `models`, and optional `optimize` aggregate commands at
startup and on
`harnessMetrics.refreshCodeBurn`. `snapshot` reads
`HARNESS_METRICS_SNAPSHOT_PATH` and launches no process. Inputs are capped at
10 MiB. Raw source, arguments, outputs, transcripts, credentials, and stderr are
never added to reports. Failures expose a stable class and retain a previous
valid snapshot when available.

Harness Metrics is MPL-2.0. CodeBurn is an optional MIT-licensed external
executable and is not bundled with server or editor.

## Diagnostic interoperability

The Rust server under [`rust/`](../rust/) is the reference implementation. It
uses full-document synchronization so unsaved buffers can participate in the
same multi-file repository analysis as files on disk. Its published diagnostics
use source `harness-lens`, stable `HLxxx` codes, severity, message, and UTF-16
ranges. Any conforming editor client—including Error Lens in VS Code—can display
them without a private protocol.
