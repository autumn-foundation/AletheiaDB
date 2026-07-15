import { defineConfig } from 'tsup';

// Dual ESM + CJS emit with type declarations. Node >=18 / standard-fetch edge
// runtimes are the target; the core client has no Node-only dependencies.
export default defineConfig({
  entry: ['src/index.ts'],
  format: ['esm', 'cjs'],
  dts: true,
  sourcemap: true,
  clean: true,
  treeshake: true,
  target: 'node18',
  outExtension({ format }) {
    return { js: format === 'cjs' ? '.cjs' : '.js' };
  },
});
