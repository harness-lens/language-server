<!-- SPDX-License-Identifier: MPL-2.0 -->
<!-- Copyright © 2026 Cristian Camargo Filho -->

# harness-lens-lsp

`HL032` flags exact duplicate lines/paragraphs in the native server. Warnings
include normalization evidence and LSP related information linking to the
earlier source location. Removing the duplicate clears the warning on edit.

After `cargo build --locked`, verify the native protocol from this directory:

```bash
node ../scripts/smoke-native-lsp.mjs target/debug/harness-lens-lsp
```

On Windows, append `.exe` to the executable path. VS Code must point to this
updated binary using `harnessLens.languageServer.path`; installing under
`.harness-lens/bin` does not update an older binary under `.cargo/bin`.

Editor-neutral Language Server Protocol adapter for Harness Lens. It scans the
workspace with the Rust SDK, overlays unsaved open documents, and publishes
standard diagnostics with stable Harness Lens rule codes.

Clients may request `harnessLens/workspaceReport` for the same content-safe
analysis report. Per-file source sizes, token estimates, configured cost,
findings, and provenance remain available without moving analysis into editor
code.

Run over standard input/output:

```bash
cargo run
```

The server keeps presentation out of the core. VS Code extensions—including
Error Lens—can render the published diagnostics without Harness Lens depending
on their APIs.

The server pins an immutable revision of
[`harness-lens/sdk`](https://github.com/harness-lens/sdk), overlays unsaved
buffers on its repository discovery, and emits UTF-16 LSP ranges for diagnostics
such as `HL010` repetition and `HL020` heuristic incongruence.

## License

MPL-2.0. See [LICENSING](../LICENSING.md), [COPYRIGHT](../COPYRIGHT), and
[TRADEMARKS](../TRADEMARKS).
