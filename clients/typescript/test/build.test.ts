import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const here = dirname(fileURLToPath(import.meta.url));
const dist = resolve(here, '../dist');
const esmEntry = resolve(dist, 'index.js');
const cjsEntry = resolve(dist, 'index.cjs');

const built = existsSync(esmEntry) && existsSync(cjsEntry);

// The build must run before this test observes the dist artifacts. When dist is
// absent (e.g. `vitest` invoked before `tsup`), the suite is skipped rather than
// failing; CI runs `build` then `test` so both entry points are exercised.
describe.skipIf(!built)('dual ESM + CJS build smoke', () => {
  it('imports the ESM entry cleanly', async () => {
    const mod = (await import(esmEntry)) as { AletheiaClient?: unknown; NotFoundError?: unknown };
    expect(typeof mod.AletheiaClient).toBe('function');
    expect(typeof mod.NotFoundError).toBe('function');
  });

  it('requires the CJS entry cleanly', () => {
    const require = createRequire(import.meta.url);
    const mod = require(cjsEntry) as { AletheiaClient?: unknown; toWireTime?: unknown };
    expect(typeof mod.AletheiaClient).toBe('function');
    expect(typeof mod.toWireTime).toBe('function');
  });
});
