import { cp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(websiteDir, '../..');
const publicDir = resolve(repoRoot, 'website/public');
const stableDocsDir = resolve(repoRoot, 'website/src/content/docs');
const previewDocsSourceDir = resolve(repoRoot, 'docs/next/website/src/content/docs');
const previewDocsDir = resolve(stableDocsDir, 'preview');
const generatedVersionsDocsDir = resolve(stableDocsDir, '_versions');
const versionsDir = resolve(repoRoot, 'docs/versions');
const versionsManifestPath = resolve(versionsDir, 'manifest.json');
const generatedVersionsDataPath = resolve(repoRoot, 'website/src/data/docs-versions.json');
const previewConfigReferenceSource = resolve(
  repoRoot,
  'docs/next/website/src/data/config-reference.json',
);
const previewConfigReferenceDestination = resolve(
  repoRoot,
  'website/src/data/config-reference-preview.json',
);
const generatedVersionReferencesPath = resolve(
  repoRoot,
  'website/src/data/config-reference-versions.json',
);

if (process.argv[2] === '--rewrite-preview-doc-fixture') {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  process.stdout.write(rewritePreviewDocContent(Buffer.concat(chunks).toString('utf8')));
} else if (process.argv[2] === '--rewrite-version-doc-fixture') {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  process.stdout.write(
    rewriteVersionDocContent(Buffer.concat(chunks).toString('utf8'), {
      version: process.argv[3] ?? '0.7.5',
      tag: `v${process.argv[3] ?? '0.7.5'}`,
      sourceRoot: 'docs/next/website/src/content/docs',
      relativePath: 'index.mdx',
    }),
  );
} else {
  await preparePublicAssets();
  await prepareDocs();
}

async function preparePublicAssets() {
  await rm(publicDir, { recursive: true, force: true });
  await mkdir(publicDir, { recursive: true });

  for (const file of [
    'install.sh',
    'install.ps1',
    'agent-guide.md',
    'latest.json',
    'preview.json',
    'robots.txt',
    '_headers',
    '_redirects',
  ]) {
    const source = resolve(repoRoot, 'website', file);
    try {
      await cp(source, resolve(publicDir, file));
    } catch (error) {
      if (file !== 'preview.json' || error.code !== 'ENOENT') throw error;
    }
  }

  for (const directory of ['assets', 'css', 'agent-detection']) {
    await cp(resolve(repoRoot, 'website', directory), resolve(publicDir, directory), {
      recursive: true,
    });
  }
}

async function prepareDocs() {
  await rm(previewDocsDir, { recursive: true, force: true });
  await rm(generatedVersionsDocsDir, { recursive: true, force: true });

  await copyPreparedDocs(previewDocsSourceDir, previewDocsDir, (content, relativePath) =>
    rewritePreviewDocContent(content, relativePath),
  );
  await cp(previewConfigReferenceSource, previewConfigReferenceDestination);

  const manifest = JSON.parse(await readFile(versionsManifestPath, 'utf8'));
  if (manifest.schema_version !== 1 || typeof manifest.current !== 'string') {
    throw new Error(`${versionsManifestPath} has an unsupported schema`);
  }

  const scopes = {
    stable: await collectDocsScope(stableDocsDir, new Set(['preview', '_versions'])),
    preview: await collectDocsScope(previewDocsSourceDir),
  };
  const configReferences = {};

  for (const entry of manifest.versions) {
    const version = entry.version;
    const snapshotDocsRoot = resolve(versionsDir, version, 'website/src/content/docs');
    const destinationRoot = resolve(generatedVersionsDocsDir, version);
    await copyPreparedDocs(snapshotDocsRoot, destinationRoot, (content, relativePath) =>
      rewriteVersionDocContent(content, {
        version,
        tag: entry.tag,
        sourceRoot: entry.source,
        relativePath,
      }),
    );
    scopes[version] = await collectDocsScope(snapshotDocsRoot);

    const referencePath = resolve(
      versionsDir,
      version,
      'website/src/data/config-reference.json',
    );
    try {
      configReferences[version] = JSON.parse(await readFile(referencePath, 'utf8'));
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }

  await writeFile(
    generatedVersionsDataPath,
    `${JSON.stringify({ ...manifest, scopes }, null, 2)}\n`,
    'utf8',
  );
  await writeFile(
    generatedVersionReferencesPath,
    `${JSON.stringify(configReferences, null, 2)}\n`,
    'utf8',
  );
}

async function copyPreparedDocs(sourceDir, destinationDir, rewrite, pathPrefix = '') {
  await mkdir(destinationDir, { recursive: true });
  for (const entry of await readdir(sourceDir, { withFileTypes: true })) {
    const source = join(sourceDir, entry.name);
    const destination = join(destinationDir, entry.name);
    const relativePath = pathPrefix ? `${pathPrefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      await copyPreparedDocs(source, destination, rewrite, relativePath);
      continue;
    }
    if (!entry.isFile()) continue;

    if (!['.md', '.mdx'].includes(extname(entry.name).toLowerCase())) {
      await cp(source, destination);
      continue;
    }
    const content = await readFile(source, 'utf8');
    await writeFile(destination, rewrite(content, relativePath), 'utf8');
  }
}

async function collectDocsScope(sourceDir, excludedDirectories = new Set()) {
  const locales = { root: [], ja: [], 'zh-cn': [] };

  async function walk(directory, prefix = '') {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (entry.isDirectory() && excludedDirectories.has(entry.name)) continue;
      const path = join(directory, entry.name);
      const relativePath = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        await walk(path, relativePath);
        continue;
      }
      if (!entry.isFile() || !['.md', '.mdx'].includes(extname(entry.name))) continue;

      const segments = relativePath.replace(/\.(md|mdx)$/i, '').split('/');
      const locale = segments[0] === 'ja' || segments[0] === 'zh-cn' ? segments.shift() : 'root';
      const page = segments.join('/').replace(/(^|\/)index$/, '').replace(/\/$/, '');
      locales[locale].push(page);
    }
  }

  await walk(sourceDir);
  for (const pages of Object.values(locales)) pages.sort();
  for (const locale of ['ja', 'zh-cn']) {
    if (locales[locale].length === 0) locales[locale] = [...locales.root];
  }
  return { locales };
}

export function rewritePreviewDocContent(content, relativePath = '') {
  const rewritten = rewriteRelativeDocPaths(
    content.replaceAll('/docs/', '/docs/preview/'),
    1,
  );
  const withEditLink = setGeneratedEditUrl(
    rewritten,
    `https://github.com/ogulcancelik/herdr/edit/master/docs/next/website/src/content/docs/${relativePath}`,
  );
  return insertPreviewNotice(withEditLink, relativePath);
}

export function rewriteVersionDocContent(content, { version, tag, sourceRoot, relativePath }) {
  const taggedContent = content
    .replaceAll('/docs/', `/docs/${version}/`)
    .replaceAll(
      'https://github.com/ogulcancelik/herdr/blob/master/',
      `https://github.com/ogulcancelik/herdr/blob/${tag}/`,
    )
    .replaceAll(
      'https://raw.githubusercontent.com/ogulcancelik/herdr/master/',
      `https://raw.githubusercontent.com/ogulcancelik/herdr/${tag}/`,
    );
  const rewritten = rewriteRelativeDocPaths(taggedContent, 2);
  return setGeneratedEditUrl(
    rewritten,
    `https://github.com/ogulcancelik/herdr/blob/${tag}/${sourceRoot}/${relativePath}`,
  );
}

function rewriteRelativeDocPaths(content, extraDepth) {
  const parents = '../'.repeat(extraDepth);
  return content
    .replace(/((?:\.\.\/)+)(?=public\/)/g, `$1${parents}`)
    .replace(/^(import .*from\s+['"])(?=(?:\.\.\/)+components\/)/gm, `$1${parents}`);
}

function setGeneratedEditUrl(content, editUrl) {
  if (!content.startsWith('---\n') || /^editUrl:/m.test(content)) return content;
  return content.replace(/^---\n/, `---\neditUrl: ${editUrl}\n`);
}

function insertPreviewNotice(content, relativePath) {
  const notice = [
    '> Next docs describe unreleased work from `master`. Stable docs remain at [/docs/](/docs/).',
    '',
    '',
  ].join('\n');
  const indexPrefix =
    relativePath === 'index.mdx'
      ? content.replace('title: Herdr documentation', 'title: Herdr next documentation')
      : content;
  const frontmatter = indexPrefix.match(/^---\n[\s\S]*?\n---\n/);
  if (!frontmatter) {
    return insertNoticeAfterImports(indexPrefix, notice);
  }
  const body = indexPrefix.slice(frontmatter[0].length);
  return `${frontmatter[0]}\n${insertNoticeAfterImports(body, notice)}`;
}

function insertNoticeAfterImports(content, notice) {
  const imports = content.match(/^(\s*import .+?;\n)+\s*/);
  if (!imports) {
    return `${notice}${content}`;
  }
  return `${imports[0]}${notice}${content.slice(imports[0].length)}`;
}
