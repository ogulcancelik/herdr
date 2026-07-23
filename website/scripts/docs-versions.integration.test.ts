import { afterEach, describe, expect, test } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const script = resolve(import.meta.dir, 'docs-versions.mjs');
const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe('documentation release publishing', () => {
  test('snapshots tagged next docs and promotes the same content', async () => {
    const root = await mkdtemp(join(tmpdir(), 'herdr-docs-'));
    temporaryDirectories.push(root);
    await write(root, 'website/src/content/docs/index.mdx', 'stable docs\n');
    await write(root, 'website/src/data/config-reference.json', '{"stable":true}\n');
    await write(root, 'website/latest.json', '{"version":"0.9.0"}\n');
    await write(root, 'README.md', 'stable readme\n');
    await write(root, 'docs/next/website/src/content/docs/index.mdx', 'next docs\n');
    await write(root, 'docs/next/website/src/data/config-reference.json', '{"next":true}\n');
    await write(root, 'docs/next/README.md', 'next readme\n');

    git(root, ['init', '-q']);
    git(root, ['config', 'user.email', 'test@example.com']);
    git(root, ['config', 'user.name', 'Test']);
    git(root, ['add', '.']);
    git(root, ['commit', '-qm', 'release fixture']);
    git(root, ['tag', 'v1.0.0']);

    runScript(root, ['publish', 'v1.0.0']);

    expect(await read(root, 'website/src/content/docs/index.mdx')).toBe('next docs\n');
    expect(await read(root, 'website/src/data/config-reference.json')).toBe('{"next":true}\n');
    expect(await read(root, 'README.md')).toBe('next readme\n');
    expect(await read(root, 'docs/versions/1.0.0/website/src/content/docs/index.mdx')).toBe('next docs\n');

    const manifest = JSON.parse(await read(root, 'docs/versions/manifest.json'));
    expect(manifest.current).toBe('1.0.0');
    expect(manifest.versions[0]).toMatchObject({
      version: '1.0.0',
      tag: 'v1.0.0',
      source: 'docs/next/website/src/content/docs',
    });

    await write(root, 'website/latest.json', '{"version":"1.0.0"}\n');
    runScript(root, ['check']);
  });
});

async function write(root: string, path: string, content: string) {
  const destination = resolve(root, path);
  await mkdir(resolve(destination, '..'), { recursive: true });
  await writeFile(destination, content, 'utf8');
}

async function read(root: string, path: string) {
  return readFile(resolve(root, path), 'utf8');
}

function git(root: string, args: string[]) {
  execFileSync('git', args, { cwd: root, stdio: 'pipe' });
}

function runScript(root: string, args: string[]) {
  execFileSync('node', [script, ...args], {
    cwd: root,
    env: { ...process.env, HERDR_DOCS_REPO_ROOT: root },
    stdio: 'pipe',
  });
}
