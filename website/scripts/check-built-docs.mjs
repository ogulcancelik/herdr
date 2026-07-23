import { access, readFile, readdir, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const distDir = resolve(websiteDir, 'dist');
const nonCanonicalDocsUrl = /https:\/\/herdr\.dev\/(?:ja\/|zh-cn\/)?docs\/(?:preview|\d+\.\d+\.\d+)(?:\/|<)/;
const versions = JSON.parse(
  await readFile(resolve(websiteDir, 'src/data/docs-versions.json'), 'utf8'),
);

for (const entry of versions.versions) {
  const scope = versions.scopes[entry.version];
  if (!scope) throw new Error(`missing generated scope for ${entry.version}`);
  for (const [locale, pages] of Object.entries(scope.locales)) {
    const localePrefix = locale === 'root' ? '' : `${locale}/`;
    for (const page of pages) {
      const output = resolve(
        distDir,
        localePrefix,
        'docs',
        entry.version,
        page,
        'index.html',
      );
      await access(output);
    }
  }
}

const stable = await readFile(resolve(distDir, 'docs/index.html'), 'utf8');
const preview = await readFile(resolve(distDir, 'docs/preview/index.html'), 'utf8');
const archived = await readFile(
  resolve(distDir, 'docs', versions.current, 'index.html'),
  'utf8',
);

assertIncludes(stable, 'data-pagefind-filter="version[content]" content="stable"');
if (stable.includes('name="robots" content="noindex')) {
  throw new Error('stable docs must remain indexable');
}
assertIncludes(preview, 'data-pagefind-filter="version[content]" content="preview"');
assertIncludes(preview, 'name="robots" content="noindex, nofollow"');
assertIncludes(archived, `data-pagefind-filter="version[content]" content="${versions.current}"`);
assertIncludes(archived, 'name="robots" content="noindex, nofollow"');
if (archived.includes(`This page documents Herdr ${versions.current}`)) {
  throw new Error('the current immutable snapshot must not be labeled as outdated');
}
const versionSelect = stable.match(/<select[^>]*aria-label="Documentation version"[^>]*>([\s\S]*?)<\/select>/)?.[1];
if (!versionSelect) throw new Error('stable docs are missing the version selector');
if (versionSelect.includes(`value="/docs/${versions.current}/"`)) {
  throw new Error('the current release is duplicated in the version selector');
}

const previous = versions.versions.find((entry) => entry.version !== versions.current);
if (previous) {
  const previousArchive = await readFile(
    resolve(distDir, 'docs', previous.version, 'index.html'),
    'utf8',
  );
  assertIncludes(previousArchive, `This page documents Herdr ${previous.version}`);
}

const sitemap = await readFile(resolve(distDir, 'sitemap-0.xml'), 'utf8');
assertIncludes(sitemap, 'https://herdr.dev/docs/');
if (nonCanonicalDocsUrl.test(sitemap)) {
  throw new Error('preview or immutable documentation URLs must not appear in the sitemap');
}

const build = await inspectFiles(distDir);
if (build.count > 20_000) {
  throw new Error(`website build has ${build.count} files, exceeding the Cloudflare Pages free-plan limit`);
}
if (build.largest.bytes > 25 * 1024 * 1024) {
  throw new Error(`website asset ${build.largest.path} is ${build.largest.bytes} bytes, exceeding Cloudflare Pages' 25 MiB limit`);
}
process.stdout.write(`validated ${versions.versions.length} documentation versions in ${build.count} website files\n`);

function assertIncludes(content, expected) {
  if (!content.includes(expected)) throw new Error(`built documentation is missing ${expected}`);
}

async function inspectFiles(directory, root = directory) {
  const result = { count: 0, largest: { path: '', bytes: 0 } };
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      const nested = await inspectFiles(path, root);
      result.count += nested.count;
      if (nested.largest.bytes > result.largest.bytes) result.largest = nested.largest;
    } else if (entry.isFile()) {
      result.count += 1;
      const { size } = await stat(path);
      if (size > result.largest.bytes) {
        result.largest = { path: path.slice(root.length + 1), bytes: size };
      }
    }
  }
  return result;
}
