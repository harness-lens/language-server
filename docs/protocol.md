> SPDX-License-Identifier: MPL-2.0
> Copyright © 2026 Cristian Camargo Filho

# Protocol scope

Reference Rust capabilities:

- full-document synchronization for unsaved-buffer overlays;
- repository discovery and diagnostics for recognized harness files;
- cross-file analysis within the deepest matching workspace root;
- stable rule codes and evidence ranges;
- warning/error severity mapping.

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
  "rootUri": "file:///workspace"
}
```

`rootUri` is optional. When omitted, the response contains one report per
initialized workspace root. Open editor documents overlay filesystem content
without mutating history.

Response envelope:

```json
{
  "schemaVersion": 1,
  "reports": []
}
```

Each report retains Core's schema version, completeness reasons, content-free
source records, findings, per-file and aggregate metrics, normalized scores,
and observable plugin executions. Source spans remain UTF-8 byte ranges in the
report; only diagnostic adapters convert positions to UTF-16.

## Diagnostic interoperability

The Rust server under [`rust/`](../rust/) is the reference implementation. It
uses full-document synchronization so unsaved buffers can participate in the
same multi-file repository analysis as files on disk. Its published diagnostics
use source `harness-lens`, stable `HLxxx` codes, severity, message, and UTF-16
ranges. Any conforming editor client—including Error Lens in VS Code—can display
them without a private protocol.
