> SPDX-License-Identifier: MPL-2.0
> Copyright © 2026 Cristian Camargo Filho

# Publishing

Publish `@harness-lens/core@0.0.1` first. Publish this package interactively once, then configure npm trusted publishing for `.github/workflows/publish.yml`.

```bash
npm login
npm publish --access public --provenance
```

Never commit npm tokens.
