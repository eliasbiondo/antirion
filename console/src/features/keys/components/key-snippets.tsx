'use client';

import { useState } from 'react';

import { Segmented } from '@/components/ui/form';
import { CopyButton } from '@/components/ui/misc';
import { chatUrl, useSetup } from '@/features/overview/setup';
import { useShell } from '@/stores/shell';

type Language = 'curl' | 'python' | 'node';

/** The snippets never contain the secret: they read it from an environment variable. */
export function keySnippet(
  language: Language,
  dataPlaneUrl: string | null | undefined,
  model: string | null = null,
): string {
  const url = chatUrl(dataPlaneUrl);
  const name = model ?? 'your-model';
  const base = url.replace(/\/chat\/completions$/, '');
  if (language === 'python')
    return [
      'import os',
      'from openai import OpenAI',
      '',
      `client = OpenAI(base_url="${base}", api_key=os.environ["ANTIRION_API_KEY"])`,
      'reply = client.chat.completions.create(',
      `    model="${name}",`,
      '    messages=[{"role": "user", "content": "Hello"}],',
      ')',
    ].join('\n');
  if (language === 'node')
    return [
      "import OpenAI from 'openai';",
      '',
      `const client = new OpenAI({ baseURL: '${base}', apiKey: process.env.ANTIRION_API_KEY });`,
      'const reply = await client.chat.completions.create({',
      `  model: '${name}',`,
      "  messages: [{ role: 'user', content: 'Hello' }],",
      '});',
    ].join('\n');
  return [
    `curl ${url} \\`,
    '  -H "Authorization: Bearer $ANTIRION_API_KEY" \\',
    '  -H "Content-Type: application/json" \\',
    `  -d '{"model": "${name}", "messages": [{"role": "user", "content": "Hello"}]}'`,
  ].join('\n');
}

/** "Use it": how an app sends a request with the new key, in curl, Python or Node. */
export function KeySnippets() {
  const [language, setLanguage] = useState<Language>('curl');
  const dataPlaneUrl = useShell((s) => s.status?.data_plane_url ?? null);
  const model = useSetup().modelName;
  const snippet = keySnippet(language, dataPlaneUrl, model);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3">
        <span className="text-ui font-medium text-fg">Use it</span>
        <Segmented<Language>
          label="Snippet language"
          value={language}
          onChange={setLanguage}
          options={[
            { value: 'curl', label: 'curl' },
            { value: 'python', label: 'Python' },
            { value: 'node', label: 'Node' },
          ]}
        />
      </div>
      <div className="relative rounded-control border border-line bg-sunken">
        <pre className="mono overflow-x-auto px-3 py-2.5 pr-10 text-caption leading-5 text-fg">{snippet}</pre>
        <span className="absolute top-1.5 right-1.5">
          <CopyButton value={snippet} label="Copy the snippet" />
        </span>
      </div>
      <p className="text-caption text-muted">
        Set <span className="mono">ANTIRION_API_KEY</span> to the key in your app&apos;s environment.
        {dataPlaneUrl ? null : ' Replace the address with your gateway URL.'}
      </p>
    </div>
  );
}
