# herdr website

The homepage is `index.html`. The documentation source is in `src/content/docs/` and is rendered by Astro Starlight.

```bash
bun install
bun run dev
bun run build
```

The build output is `dist/`. Configure Cloudflare Pages to use `website` as the project root and publish `dist`.

Stable docs live in `src/content/docs/`. Unreleased docs live in `../docs/next/website/src/content/docs/` and are generated at `/docs/preview/`. Immutable release snapshots live in `../docs/versions/` and are generated at `/docs/<version>/`.

Do not promote docs manually before a release. After the GitHub Release succeeds, release CI runs:

```bash
node website/scripts/docs-versions.mjs publish <tag>
```

This snapshots the tagged next docs and promotes the same tagged content to stable before the website deploy. Use `node website/scripts/docs-versions.mjs check` to validate committed snapshots against their release tags.
