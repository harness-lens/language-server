> SPDX-License-Identifier: MPL-2.0
> Copyright © 2026 Cristian Camargo Filho

# @harness-lens/language-server

Language Server Protocol adapter for Harness Lens findings. The native server in
[`rust/`](rust/) is the reference implementation; the existing TypeScript
package remains available to npm clients.

```bash
npx @harness-lens/language-server --stdio
```

The Rust server discovers the workspace hierarchy, overlays unsaved open harness
documents, evaluates cross-file findings, and publishes evidence-backed warnings
and errors for open and closed harness files with stable `HLxxx` codes and
precise UTF-16 ranges. Optional CodeBurn
aggregates add `HMxxx` diagnostics, hover, and code lenses through Harness
Metrics. Standard clients render these without a private editor protocol.

Editors remain adapters: validation stays in `@harness-lens/core`. AI interpretation is not part of diagnostics or deterministic scoring.

Runtime mode defaults to `off`; no capture process starts. Set
`HARNESS_METRICS_MODE=live` for bounded CodeBurn aggregate capture, or
`HARNESS_METRICS_MODE=snapshot` with `HARNESS_METRICS_SNAPSHOT_PATH` to read a
canonical aggregate snapshot without process launch. Live mode accepts
`HARNESS_METRICS_CODEBURN_EXECUTABLE` and `HARNESS_METRICS_CODEBURN_PERIOD`
(default `30days`). Failures expose stable classes only, retain a previous valid
snapshot, and never persist raw stderr.

Optional live access also requires trusted, non-virtual workspace policy.
Clients should send `initializationOptions.harnessLens.workspaceTrusted` and
`virtualWorkspace`; non-editor clients may use
`HARNESS_LENS_WORKSPACE_TRUSTED=true`. `harnessLens/providerCatalog` exposes
compiled-in provider status. `harnessLens/providerAggregate` returns a bounded
Native report beside namespaced provider measurements without changing Native
scores or serializing source content.

Bootstrap order: publish `@harness-lens/core@0.0.1` before this package.

## Ecosystem

- [Core](https://github.com/harness-lens/core)
- [SDK](https://github.com/harness-lens/sdk)
- [CLI](https://github.com/harness-lens/cli)
- [VS Code client](https://github.com/harness-lens/harness-lens-vscode)
- [Project hub](https://github.com/harness-lens/harness-lens)

## Development

```bash
npm install
npm test
npm run check

cd rust
cargo test --locked
```

## License

Early namespace-reservation versions used BSD-3-Clause. The official functional
implementation is licensed under MPL-2.0. When Covered Software is distributed,
modified MPL-covered files must remain available in Source Code Form under the
license. See [LICENSING](LICENSING.md), [COPYRIGHT](COPYRIGHT), and
[TRADEMARKS](TRADEMARKS).
