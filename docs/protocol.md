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

## Diagnostic interoperability

The Rust server under [`rust/`](../rust/) is the reference implementation. It
uses full-document synchronization so unsaved buffers can participate in the
same multi-file repository analysis as files on disk. Its published diagnostics
use source `harness-lens`, stable `HLxxx` codes, severity, message, and UTF-16
ranges. Any conforming editor client—including Error Lens in VS Code—can display
them without a private protocol.
