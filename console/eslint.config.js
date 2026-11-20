// @ts-check
import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist/', 'coverage/', 'node_modules/', 'playwright-report/', 'test-results/'] },
  js.configs.recommended,
  ...tseslint.configs.strict,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: { globals: { ...globals.browser } },
    plugins: { 'react-hooks': reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // Server-provided text must never be rendered as HTML.
      'no-restricted-syntax': [
        'error',
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: 'Do not render HTML; display server text as plain text.',
        },
        {
          selector: "MemberExpression[property.name='innerHTML']",
          message: 'Do not assign HTML; display server text as plain text.',
        },
      ],
      // Identifiers and revisions must stay exact strings.
      'no-restricted-globals': [
        'error',
        { name: 'parseInt', message: 'Keep identifiers and revisions as strings or BigInt.' },
        { name: 'parseFloat', message: 'Keep identifiers and revisions as strings or BigInt.' },
      ],
    },
  },
  {
    files: ['src/api/generated/**'],
    rules: { '@typescript-eslint/no-explicit-any': 'off' },
  },
  {
    files: ['scripts/**/*.mjs', 'e2e/**/*.mjs', 'eslint.config.js'],
    languageOptions: { globals: { ...globals.node } },
  },
  {
    files: ['test/**', 'e2e/**', '*.config.ts'],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
    // Tests assert presence explicitly; a failed assertion is the desired error.
    rules: { '@typescript-eslint/no-non-null-assertion': 'off' },
  },
);
