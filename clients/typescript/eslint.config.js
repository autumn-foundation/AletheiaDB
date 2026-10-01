// Flat ESLint config. The load-bearing rule for Issue #3369's success metric is
// `@typescript-eslint/no-explicit-any: error` — 0 `any` in the public surface.
import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    // `scripts/` are plain Node smoke runners (not part of the typed surface).
    ignores: ['dist/**', 'node_modules/**', 'coverage/**', 'scripts/**'],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    rules: {
      // Success metric: no `any` may appear in the published type surface.
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      '@typescript-eslint/consistent-type-imports': 'error',
    },
  },
  {
    // Tests still ban `any`.
    files: ['test/**/*.ts'],
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
    },
  },
);
