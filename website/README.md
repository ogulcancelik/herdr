# herdr website

The homepage is `index.html`. The documentation source is in `src/content/docs/` and is rendered by Astro Starlight.

```bash
bun install
bun run dev
bun run build
```

The build output is `dist/`. Configure Cloudflare Pages to use `website` as the project root and publish `dist`.
