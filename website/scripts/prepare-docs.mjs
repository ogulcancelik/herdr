import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(websiteDir, '../..');
const publicDir = resolve(repoRoot, 'website/public');
const stableDocsDir = resolve(repoRoot, 'website/src/content/docs');
const previewDocsSourceDir = resolve(repoRoot, 'docs/next/website/src/content/docs');
const previewDocsDir = resolve(stableDocsDir, 'preview');
const previewConfigReferenceSource = resolve(
  repoRoot,
  'docs/next/website/src/data/config-reference.json',
);
const previewConfigReferenceDestination = resolve(
  repoRoot,
  'website/src/data/config-reference-preview.json',
);

if (process.argv[2] === '--rewrite-preview-doc-fixture') {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  process.stdout.write(rewritePreviewDocContent(Buffer.concat(chunks).toString('utf8')));
} else {
  await preparePublicAssets();
  await preparePreviewDocs();
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

async function preparePreviewDocs() {
  await rm(previewDocsDir, { recursive: true, force: true });
  await copyPreviewDocs(previewDocsSourceDir, previewDocsDir);
  await cp(previewConfigReferenceSource, previewConfigReferenceDestination);
}

async function copyPreviewDocs(sourceDir, destinationDir) {
  await mkdir(destinationDir, { recursive: true });
  for (const entry of await readdir(sourceDir, { withFileTypes: true })) {
    const source = join(sourceDir, entry.name);
    const destination = join(destinationDir, entry.name);
    if (entry.isDirectory()) {
      await copyPreviewDocs(source, destination);
      continue;
    }
    if (!entry.isFile()) continue;

    const content = await readFile(source, 'utf8');
    const relativePath = relative(previewDocsSourceDir, source);
    await writeFile(destination, rewritePreviewDocContent(content, relativePath), 'utf8');
  }
}

export function rewritePreviewDocContent(content, relativePath = '') {
  const rewritten = content
    .replaceAll('/docs/', '/docs/preview/')
    .replaceAll('../../../public/', '../../../../public/')
    // Preview docs live one directory deeper than stable docs, so component
    // imports need one more parent segment regardless of locale depth. Only
    // MDX import lines are rewritten; prose mentioning relative paths is not.
    .replace(/^(import .*from\s+['"])(?=(?:\.\.\/)+components\/)/gm, '$1../');
  return insertPreviewNotice(rewritten, relativePath);
}

function insertPreviewNotice(content, relativePath) {
  const notice = [
    '> Preview docs describe unreleased preview builds. Stable docs remain at [/docs/](/docs/).',
    '',
    '',
  ].join('\n');
  const indexPrefix =
    relativePath === 'index.mdx'
      ? content.replace('title: Herdr documentation', 'title: Herdr preview documentation')
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
