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
and errors with stable `HLxxx` codes and precise UTF-16 ranges. Because these are
standard LSP diagnostics, extensions such as Error Lens can render them inline
without an Error Lens dependency.

Editors remain adapters: validation stays in `@harness-lens/core`. AI interpretation is not part of diagnostics or deterministic scoring.

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
