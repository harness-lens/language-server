> SPDX-License-Identifier: MPL-2.0
> Copyright © 2026 Cristian Camargo Filho

# How to contribute

Read the central [ecosystem contribution flow](https://github.com/harness-lens/harness-lens/blob/main/docs/architecture.md#how-to-contribute),
[architecture rules](https://github.com/harness-lens/harness-lens/blob/main/docs/architecture.md#architecture-rules),
and [LSP-visible rule path](https://github.com/harness-lens/harness-lens/blob/main/docs/architecture.md#adding-an-lsp-visible-rule).
This repository owns protocol lifecycle, workspace overlays, content-safe custom
requests, diagnostic mapping, related locations, and UTF-8-to-UTF-16 conversion.
Rule behavior belongs in Core. See the [protocol guide](docs/protocol.md).

Run:

```bash
npm ci
npm test
npm run check

cd rust
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --locked
node ../scripts/smoke-native-lsp.mjs target/debug/harness-lens-lsp
```

## Licensing contributions

Contributions intentionally submitted to this repository are provided under
MPL-2.0. You must have the necessary rights to submit the work. When Covered
Software is distributed, modifications to MPL-covered files remain subject to
the Source Code Form obligations in the license.
