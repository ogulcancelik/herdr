import { cp, mkdir, rm } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(websiteDir, '../..');
const publicDir = resolve(repoRoot, 'website/public');

await preparePublicAssets();

async function preparePublicAssets() {
  await rm(publicDir, { recursive: true, force: true });
  await mkdir(publicDir, { recursive: true });

  for (const file of [
    'install.sh',
    'latest.json',
    'robots.txt',
    '_headers',
    '_redirects',
  ]) {
    await cp(resolve(repoRoot, 'website', file), resolve(publicDir, file));
  }

  for (const directory of ['assets', 'css']) {
    await cp(resolve(repoRoot, 'website', directory), resolve(publicDir, directory), {
      recursive: true,
    });
  }
}
