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

## Runtime evidence

`off` is default and performs no runtime I/O. `live` runs CodeBurn `report`,
`models`, and optional `optimize` aggregate commands at startup and on
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
