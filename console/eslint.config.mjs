import { defineConfig, globalIgnores } from 'eslint/config';
import nextVitals from 'eslint-config-next/core-web-vitals';
import nextTypescript from 'eslint-config-next/typescript';
import tseslint from 'typescript-eslint';

/**
 * The console draws its own controls (Select, Combobox, MultiSelect, Checkbox, RadioGroup,
 * Switch, DatePicker, NumberInput, Tooltip, ConfirmDialog); the browser's native ones must not come
 * back. Only the files that implement those components may use what they wrap.
 */
const NATIVE_WRAPPERS = [
  'src/components/ui/select.tsx',
  'src/components/ui/choice.tsx',
  'src/components/ui/date-picker.tsx',
  'src/components/ui/tooltip.tsx',
  'src/components/ui/form.tsx',
];
const NATIVE_INPUT_TYPES = 'checkbox|radio|date|datetime-local|month|week|time|number|range|color|file';
const noNativeControls = {
  'no-restricted-syntax': [
    'error',
    {
      selector: "JSXOpeningElement[name.name='select']",
      message: 'Use Select or Combobox from @/components/ui/form instead of a native <select>.',
    },
    {
      selector: "JSXOpeningElement[name.name='datalist']",
      message: 'Use Combobox from @/components/ui/form instead of <datalist>.',
    },
    {
      selector: `JSXOpeningElement[name.name='input'] > JSXAttribute[name.name='type'][value.value=/^(${NATIVE_INPUT_TYPES})$/]`,
      message:
        'Use the console control instead of a native input: Checkbox, RadioGroup or Switch, DatePicker, NumberInput.',
    },
    {
      selector: `JSXOpeningElement[name.name=/^(Input|Textarea)$/] > JSXAttribute[name.name='type'][value.value=/^(${NATIVE_INPUT_TYPES})$/]`,
      message: 'Use DatePicker or NumberInput instead of a native date or number field.',
    },
    {
      selector: "JSXOpeningElement[name.name=/^(?!iframe$)[a-z]/] > JSXAttribute[name.name='title']",
      message: 'Use Tooltip (or a visible label) instead of the browser tooltip from title=.',
    },
    {
      selector: "JSXOpeningElement[name.name=/^(Link|Input|Textarea)$/] > JSXAttribute[name.name='title']",
      message: 'Use Tooltip instead of the browser tooltip from title=.',
    },
  ],
  'no-restricted-globals': [
    'error',
    { name: 'confirm', message: 'Use ConfirmDialog instead of window.confirm.' },
    { name: 'alert', message: 'Use notify() or a dialog instead of window.alert.' },
    { name: 'prompt', message: 'Use a Dialog with a field instead of window.prompt.' },
  ],
  'no-restricted-properties': [
    'error',
    { object: 'window', property: 'confirm', message: 'Use ConfirmDialog instead of window.confirm.' },
    { object: 'window', property: 'alert', message: 'Use notify() or a dialog instead of window.alert.' },
    { object: 'window', property: 'prompt', message: 'Use a Dialog with a field instead of window.prompt.' },
  ],
};

export default defineConfig([
  globalIgnores([
    '.next/**',
    'out/**',
    'node_modules/**',
    'test-results/**',
    'playwright-report/**',
    'public/**',
    'src/lib/api/schema.d.ts',
    'next-env.d.ts',
  ]),
  ...nextVitals,
  ...nextTypescript,
  {
    files: ['**/*.ts', '**/*.tsx'],
    extends: [tseslint.configs.strictTypeChecked, tseslint.configs.stylisticTypeChecked],
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', ignoreRestSiblings: true },
      ],
      '@typescript-eslint/consistent-type-definitions': 'off',
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
      '@typescript-eslint/no-confusing-void-expression': ['error', { ignoreArrowShorthand: true }],
      '@typescript-eslint/no-misused-promises': ['error', { checksVoidReturn: { attributes: false } }],
      '@typescript-eslint/no-unnecessary-condition': ['error', { allowConstantLoopConditions: true }],
      '@typescript-eslint/prefer-nullish-coalescing': [
        'error',
        { ignorePrimitives: { string: true, boolean: true } },
      ],
    },
  },
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    ignores: NATIVE_WRAPPERS,
    rules: noNativeControls,
  },
  {
    // The theme script must run before first paint, and the CSP forbids inline scripts.
    files: ['src/app/layout.tsx'],
    rules: { '@next/next/no-sync-scripts': 'off' },
  },
  {
    files: ['scripts/**/*.mjs', '*.config.*', 'tests/**/*.ts'],
    rules: { '@typescript-eslint/no-floating-promises': 'off' },
  },
]);
