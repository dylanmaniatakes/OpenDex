import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../', import.meta.url));
test('the desktop bundle includes every local JavaScript import', () => {
  const config = JSON.parse(readFileSync(path.join(root, 'tauri.conf.json'), 'utf8'));
  const assets = new Set(config.build.frontendDist);
  for (const asset of assets) {
    if (!asset.endsWith('.js')) continue;
    const source = readFileSync(path.join(root, asset), 'utf8');
    for (const match of source.matchAll(/(?:from\s*|import\s*)['"](\.[^'"]+)['"]/g)) {
      const dependency = path.posix.normalize(path.posix.join(path.posix.dirname(asset), match[1]));
      assert(assets.has(dependency), `${asset} imports ${dependency}, which is missing from frontendDist`);
    }
  }
});
