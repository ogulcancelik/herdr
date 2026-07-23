import { execFileSync } from 'node:child_process';
import { mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = process.env.HERDR_DOCS_REPO_ROOT
  ? resolve(process.env.HERDR_DOCS_REPO_ROOT)
  : resolve(websiteDir, '../..');
const versionsDir = resolve(repoRoot, 'docs/versions');
const manifestPath = resolve(versionsDir, 'manifest.json');
const stableDocsDir = resolve(repoRoot, 'website/src/content/docs');
const stableReferencePath = resolve(repoRoot, 'website/src/data/config-reference.json');

const VERSION_PATTERN = /^v?(\d+\.\d+\.\d+)$/;

export function normalizeVersion(value) {
  const match = VERSION_PATTERN.exec(value);
  if (!match) throw new Error(`version must look like 0.7.5 or v0.7.5, got ${value}`);
  return match[1];
}

export function sortVersionsNewestFirst(versions) {
  return [...versions].sort((left, right) => {
    const a = left.version.split('.').map(Number);
    const b = right.version.split('.').map(Number);
    for (let index = 0; index < 3; index += 1) {
      if (a[index] !== b[index]) return b[index] - a[index];
    }
    return 0;
  });
}

function git(args, options = {}) {
  return execFileSync('git', args, {
    cwd: repoRoot,
    encoding: options.binary ? undefined : 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function gitTreesEqual(ref, left, right) {
  try {
    git(['diff', '--quiet', `${ref}:${left}`, `${ref}:${right}`]);
    return true;
  } catch {
    return false;
  }
}

function gitPathExists(ref, path) {
  try {
    git(['cat-file', '-e', `${ref}:${path}`]);
    return true;
  } catch {
    return false;
  }
}

function listGitFiles(ref, root) {
  const output = git(['ls-tree', '-r', '--name-only', ref, '--', root]);
  return output.split('\n').filter(Boolean);
}

async function extractGitTree(ref, sourceRoot, destinationRoot) {
  const files = listGitFiles(ref, sourceRoot);
  if (files.length === 0) throw new Error(`${ref}:${sourceRoot} contains no files`);

  await rm(destinationRoot, { recursive: true, force: true });
  for (const file of files) {
    const destination = resolve(destinationRoot, relative(sourceRoot, file));
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, git(['show', `${ref}:${file}`], { binary: true }));
  }
}

async function readManifest() {
  try {
    return JSON.parse(await readFile(manifestPath, 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') return { schema_version: 1, current: null, versions: [] };
    throw error;
  }
}

async function writeManifest(manifest) {
  manifest.versions = sortVersionsNewestFirst(manifest.versions).map(({ version, tag, source }) => ({
    version,
    tag,
    source,
  }));
  await mkdir(dirname(manifestPath), { recursive: true });
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
}

function releaseMetadata(tag) {
  const version = normalizeVersion(tag);
  return { version, tag: `v${version}` };
}

async function snapshotTag(tag, sourceRoot) {
  const metadata = { ...releaseMetadata(tag), source: sourceRoot };
  const destination = resolve(versionsDir, metadata.version);
  await rm(destination, { recursive: true, force: true });
  await extractGitTree(tag, sourceRoot, resolve(destination, 'website/src/content/docs'));

  const referenceSource = sourceRoot.includes('docs/next/')
    ? 'docs/next/website/src/data/config-reference.json'
    : 'website/src/data/config-reference.json';
  if (gitPathExists(tag, referenceSource)) {
    const referenceDestination = resolve(destination, 'website/src/data/config-reference.json');
    await mkdir(dirname(referenceDestination), { recursive: true });
    await writeFile(referenceDestination, git(['show', `${tag}:${referenceSource}`], { binary: true }));
  }

  return metadata;
}

export async function backfillVersions() {
  const tags = git(['tag', '--list', 'v*', '--sort=v:refname']).split('\n').filter(Boolean);
  const manifest = await readManifest();
  const entries = new Map(manifest.versions.map((entry) => [entry.version, entry]));

  for (const tag of tags) {
    const version = normalizeVersion(tag);
    if (entries.has(version) || !gitPathExists(tag, 'website/src/content/docs')) continue;
    const nextRoot = 'docs/next/website/src/content/docs';
    const sourceRoot =
      gitPathExists(tag, nextRoot) && !gitTreesEqual(tag, 'website/src/content/docs', nextRoot)
        ? nextRoot
        : 'website/src/content/docs';
    const metadata = await snapshotTag(tag, sourceRoot);
    entries.set(metadata.version, metadata);
    process.stdout.write(`snapshotted ${tag}\n`);
  }

  const current = JSON.parse(await readFile(resolve(repoRoot, 'website/latest.json'), 'utf8')).version;
  await writeManifest({
    schema_version: 1,
    current: normalizeVersion(current),
    versions: [...entries.values()],
  });
}

export async function checkVersions() {
  const manifest = await readManifest();
  if (manifest.schema_version !== 1 || typeof manifest.current !== 'string') {
    throw new Error(`${manifestPath} has an unsupported schema`);
  }

  const latest = JSON.parse(await readFile(resolve(repoRoot, 'website/latest.json'), 'utf8')).version;
  if (manifest.current !== normalizeVersion(latest)) {
    throw new Error(`docs current version ${manifest.current} does not match website latest ${latest}`);
  }

  const expectedOrder = sortVersionsNewestFirst(manifest.versions).map(({ version }) => version);
  if (JSON.stringify(manifest.versions.map(({ version }) => version)) !== JSON.stringify(expectedOrder)) {
    throw new Error('documentation versions are not sorted newest first');
  }

  const seen = new Set();
  for (const entry of manifest.versions) {
    if (seen.has(entry.version)) throw new Error(`duplicate docs version ${entry.version}`);
    seen.add(entry.version);
    if (normalizeVersion(entry.tag) !== entry.version) {
      throw new Error(`docs version ${entry.version} has mismatched tag ${entry.tag}`);
    }
    if (!['website/src/content/docs', 'docs/next/website/src/content/docs'].includes(entry.source)) {
      throw new Error(`docs version ${entry.version} has unsupported source ${entry.source}`);
    }
    const sourceRoot = entry.source;
    const snapshotRoot = resolve(versionsDir, entry.version, 'website/src/content/docs');
    const expected = listGitFiles(entry.tag, sourceRoot).map((path) => relative(sourceRoot, path)).sort();
    const actual = (await listFiles(snapshotRoot)).sort();
    if (JSON.stringify(actual) !== JSON.stringify(expected)) {
      throw new Error(`documentation file list for ${entry.version} differs from ${entry.tag}:${sourceRoot}`);
    }
    for (const relativePath of expected) {
      const actualContent = await readFile(resolve(snapshotRoot, relativePath));
      const taggedPath = `${sourceRoot}/${relativePath.split('\\').join('/')}`;
      const taggedContent = git(['show', `${entry.tag}:${taggedPath}`], { binary: true });
      if (!actualContent.equals(taggedContent)) {
        throw new Error(`${entry.version}/${relativePath} differs from ${entry.tag}:${sourceRoot}`);
      }
    }

    const referenceSource = sourceRoot.includes('docs/next/')
      ? 'docs/next/website/src/data/config-reference.json'
      : 'website/src/data/config-reference.json';
    const referenceSnapshot = resolve(
      versionsDir,
      entry.version,
      'website/src/data/config-reference.json',
    );
    if (gitPathExists(entry.tag, referenceSource)) {
      const actualReference = await readFile(referenceSnapshot);
      const taggedReference = git(['show', `${entry.tag}:${referenceSource}`], { binary: true });
      if (!actualReference.equals(taggedReference)) {
        throw new Error(`${entry.version} config reference differs from ${entry.tag}:${referenceSource}`);
      }
    } else {
      try {
        await readFile(referenceSnapshot);
        throw new Error(`${entry.version} has an unexpected config reference snapshot`);
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
    }
  }

  if (!seen.has(manifest.current)) {
    throw new Error(`current docs version ${manifest.current} has no snapshot`);
  }
  process.stdout.write(`validated ${manifest.versions.length} documentation snapshots\n`);
}

export async function publishVersion(tag) {
  const release = releaseMetadata(tag);
  if (!gitPathExists(tag, 'docs/next/website/src/content/docs')) {
    throw new Error(`${tag} does not contain staged website documentation`);
  }

  const manifest = await readManifest();
  const existing = manifest.versions.find((entry) => entry.version === release.version);
  if (existing && existing.tag !== release.tag) {
    throw new Error(`version ${release.version} is already associated with ${existing.tag}`);
  }

  const metadata = await snapshotTag(tag, 'docs/next/website/src/content/docs');
  const snapshotRoot = resolve(versionsDir, metadata.version, 'website');
  await replaceDirectory(resolve(snapshotRoot, 'src/content/docs'), stableDocsDir);

  const snapshotReference = resolve(snapshotRoot, 'src/data/config-reference.json');
  try {
    await mkdir(dirname(stableReferencePath), { recursive: true });
    await writeFile(stableReferencePath, await readFile(snapshotReference));
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await rm(stableReferencePath, { force: true });
  }

  const nextReadme = `docs/next/README.md`;
  if (gitPathExists(tag, nextReadme)) {
    await writeFile(resolve(repoRoot, 'README.md'), git(['show', `${tag}:${nextReadme}`], { binary: true }));
  }

  const entries = new Map(manifest.versions.map((entry) => [entry.version, entry]));
  entries.set(metadata.version, metadata);
  await writeManifest({
    schema_version: 1,
    current: metadata.version,
    versions: [...entries.values()],
  });
  process.stdout.write(`published documentation snapshot ${metadata.tag}\n`);
}

async function replaceDirectory(source, destination) {
  await rm(destination, { recursive: true, force: true });
  await copyDirectory(source, destination);
}

async function listFiles(root) {
  const files = [];
  async function walk(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.isFile()) files.push(relative(root, path));
    }
  }
  await walk(root);
  return files;
}

async function copyDirectory(source, destination) {
  await mkdir(destination, { recursive: true });
  for (const entry of await readdir(source, { withFileTypes: true })) {
    const sourcePath = join(source, entry.name);
    const destinationPath = join(destination, entry.name);
    if (entry.isDirectory()) {
      await copyDirectory(sourcePath, destinationPath);
    } else if (entry.isFile()) {
      await writeFile(destinationPath, await readFile(sourcePath));
    }
  }
}

async function main() {
  const [command, value] = process.argv.slice(2);
  if (command === 'backfill' && !value) {
    await backfillVersions();
    return;
  }
  if (command === 'publish' && value) {
    await publishVersion(value);
    return;
  }
  if (command === 'check' && !value) {
    await checkVersions();
    return;
  }
  throw new Error('usage: node website/scripts/docs-versions.mjs backfill | check | publish <tag>');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
