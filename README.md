# @harness-lens/language-server

Language Server Protocol adapter for Harness Lens findings.

```bash
npx @harness-lens/language-server --stdio
```

The `0.0.1` server validates recognized open harness documents and publishes evidence-backed warnings and errors with stable `HLxxx` codes. Workspace hierarchy, cross-file conflicts, code actions, and snapshot trends are planned.

Editors remain adapters: validation stays in `@harness-lens/core`. AI interpretation is not part of diagnostics or deterministic scoring.

Bootstrap order: publish `@harness-lens/core@0.0.1` before this package.

## Development

```bash
npm install
npm test
npm run check
```
