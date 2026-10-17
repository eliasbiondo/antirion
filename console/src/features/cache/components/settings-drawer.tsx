'use client';

import { useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, RadioGroup, Segmented, Select, Switch, Textarea } from '@/components/ui/form';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { modelOption } from '@/components/ui/marks';
import type { ModelResource } from '@/lib/api/types';
import { notify } from '@/stores/ui';

import {
  cacheOf,
  duration,
  ignoreFieldError,
  KEY_PRESETS,
  keyStrategyOf,
  TTLS,
  type KeyPresetId,
} from '../policy';
import { useCache } from '../store';
import { useEdited } from '@/lib/hooks';

interface Values {
  enabled: boolean;
  ttl: string;
  keyPreset: KeyPresetId;
  customFields: string;
  maxEntrySize: string;
  maxEntries: string;
  readOnly: boolean;
  semantic: boolean;
  embeddingModel: string;
  threshold: string;
  prompt: boolean;
  promptTtl: '5m' | '1h';
  failMode: 'open' | 'closed';
  perItem: boolean;
}

type Errors = Partial<Record<keyof Values, string>>;

function check(v: Values): Errors {
  const e: Errors = {};
  if (!/^\d+(s|m|h|d)$/.test(v.ttl)) e.ttl = 'Choose how long to keep responses';
  if (!/^\d+(KiB|MiB)$/.test(v.maxEntrySize)) e.maxEntrySize = 'Enter a size in KiB or MiB, such as 512KiB';
  else {
    const kib = Number(v.maxEntrySize.replace(/\D+$/, '')) * (v.maxEntrySize.endsWith('MiB') ? 1024 : 1);
    if (kib < 1 || kib > 64 * 1024) e.maxEntrySize = 'Enter a size between 1 KiB and 64 MiB';
  }
  if (v.maxEntries.trim() && !/^[1-9]\d*$/.test(v.maxEntries.trim()))
    e.maxEntries = 'Enter a whole number, or leave it empty for no cap';
  if (v.semantic) {
    const t = Number(v.threshold);
    if (!Number.isFinite(t) || t < 0.8 || t > 1) e.threshold = 'Enter a number between 0.80 and 1.00';
    if (!v.embeddingModel) e.embeddingModel = 'Choose the model that turns prompts into vectors';
  }
  if (v.keyPreset === 'custom') {
    const bad = fieldsOf(v.customFields)
      .map((f) => ignoreFieldError(f))
      .find((x) => x !== null);
    if (bad) e.customFields = bad;
  }
  return e;
}

function fieldsOf(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

/**
 * A Model's cache settings: response caching with its lifetime, key strategy, sizes and read-only
 * mode, similar-prompt matching with its embedding model and threshold, provider prompt caching,
 * what happens when the cache can't be reached, and per-item embedding caching. Only the fields that
 * changed are written, as a merge patch of the Model's `policies.cache`.
 */
export function SettingsDrawer({ model, onClose }: { model: ModelResource; onClose: () => void }) {
  const save = useCache((s) => s.setModelCache);
  const models = useCache((s) => s.models.data);
  const c = cacheOf(model);
  const strategy = keyStrategyOf(c);
  const initial: Values = {
    enabled: c.enabled === true,
    ttl: c.ttl ?? '1h',
    keyPreset: strategy.id,
    customFields: strategy.fields.join('\n'),
    maxEntrySize: c.max_entry_size ?? '1MiB',
    maxEntries: c.max_entries ? String(c.max_entries) : '',
    readOnly: c.read_only === true,
    semantic: c.semantic?.enabled === true,
    embeddingModel: c.semantic?.embedding_model ?? '',
    threshold: (c.semantic?.threshold ?? 0.95).toFixed(2),
    prompt: c.prompt_caching?.auto_inject === true,
    promptTtl: c.prompt_caching?.ttl ?? '5m',
    failMode: c.fail_mode ?? 'open',
    perItem: c.embeddings?.per_item === true,
  };
  const [v, setV] = useState<Values>(initial);
  const [errors, setErrors] = useState<Errors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(v);
  const embedders = useMemo(
    () =>
      (models ?? [])
        .filter((m) => m.name !== model.name)
        .map((m) => modelOption(m, m.spec.display_name ?? m.name)),
    [models, model.name],
  );
  const put = (patch: Partial<Values>) => setV((prev) => ({ ...prev, ...patch }));

  /** The merge patch: each managed member whose value differs from the Model's current one. */
  function patchOf(): Record<string, unknown> {
    const p: Record<string, unknown> = {};
    if (v.enabled !== (c.enabled === true)) p.enabled = v.enabled;
    if (v.ttl !== (c.ttl ?? '1h')) p.ttl = v.ttl;
    if (v.maxEntrySize !== (c.max_entry_size ?? '1MiB')) p.max_entry_size = v.maxEntrySize;
    const maxEntries = v.maxEntries.trim() ? Number(v.maxEntries.trim()) : null;
    if (maxEntries !== (c.max_entries ?? null)) p.max_entries = maxEntries;
    if (v.readOnly !== (c.read_only === true)) p.read_only = v.readOnly;
    const fields =
      v.keyPreset === 'custom'
        ? fieldsOf(v.customFields)
        : [...(KEY_PRESETS.find((k) => k.id === v.keyPreset)?.fields ?? [])];
    if (!same(fields, c.key?.ignore_fields ?? [])) p.key = { ignore_fields: fields };
    const semantic = {
      enabled: v.semantic,
      ...(v.embeddingModel ? { embedding_model: v.embeddingModel } : {}),
      threshold: Number(Number(v.threshold).toFixed(2)),
    };
    const semanticBefore = {
      enabled: c.semantic?.enabled === true,
      ...(c.semantic?.embedding_model ? { embedding_model: c.semantic.embedding_model } : {}),
      threshold: c.semantic?.threshold ?? 0.95,
    };
    if (v.semantic || c.semantic) {
      if (!v.semantic) {
        if (c.semantic?.enabled === true) p.semantic = { enabled: false };
      } else if (!same(semantic, semanticBefore)) p.semantic = semantic;
    }
    if (
      v.prompt !== (c.prompt_caching?.auto_inject === true) ||
      (v.prompt && v.promptTtl !== (c.prompt_caching?.ttl ?? '5m'))
    )
      p.prompt_caching = { auto_inject: v.prompt, ...(v.prompt ? { ttl: v.promptTtl } : {}) };
    if (v.failMode !== (c.fail_mode ?? 'open')) p.fail_mode = v.failMode;
    if (v.perItem !== (c.embeddings?.per_item === true)) p.embeddings = { per_item: v.perItem };
    return p;
  }

  async function submit() {
    const found = check(v);
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    const patch = patchOf();
    if (Object.keys(patch).length === 0) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await save(model.name, patch);
      notify('success', 'Cache settings saved', model.name);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const presetOptions = [
    ...KEY_PRESETS.map((p) => ({ value: p.id, label: p.label, description: p.description })),
    {
      value: 'custom' as KeyPresetId,
      label: 'Custom',
      description: 'List body fields to leave out of the key. Only fields that never change the answer.',
    },
  ];

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title={`Cache settings for ${model.name}`}
      subtitle="These win over team and key settings; what you leave unset comes from them."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Save settings
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-6">
        <InlineError error={error} />
        <section aria-labelledby="cs-responses" className="flex flex-col gap-4">
          <h3 id="cs-responses" className="text-ui font-semibold text-fg">
            Cached responses
          </h3>
          <Switch
            label="Answer repeated requests from the cache"
            checked={v.enabled}
            onChange={(enabled) => put({ enabled })}
          />
          <Field
            label="Keep responses for"
            error={errors.ttl}
            hint="Time from when a response is cached. Hits don't extend it."
          >
            <Segmented<string>
              label="Keep responses for"
              value={v.ttl}
              onChange={(ttl) => put({ ttl })}
              options={
                TTLS.some((t) => t.value === initial.ttl)
                  ? TTLS
                  : [...TTLS, { value: initial.ttl, label: duration(initial.ttl) }]
              }
            />
          </Field>
          <Field label="What makes two requests the same">
            <RadioGroup<KeyPresetId>
              label="What makes two requests the same"
              value={v.keyPreset}
              onChange={(keyPreset) => put({ keyPreset })}
              options={presetOptions}
            />
          </Field>
          {v.keyPreset === 'custom' ? (
            <Field
              label="Body fields to leave out of the key"
              error={errors.customFields}
              hint="One JSON Pointer per line, such as /request_id. The prompt, tools and sampling settings always stay in the key."
            >
              <Textarea
                mono
                rows={3}
                value={v.customFields}
                onChange={(e) => put({ customFields: e.target.value })}
              />
            </Field>
          ) : null}
          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              label="Largest response to cache"
              error={errors.maxEntrySize}
              hint="Larger responses pass through uncached."
            >
              <Input mono value={v.maxEntrySize} onChange={(e) => put({ maxEntrySize: e.target.value })} />
            </Field>
            <Field
              label="Most responses to keep"
              optional
              error={errors.maxEntries}
              hint="The least recently used go first. Empty for no cap."
            >
              <Input
                mono
                inputMode="numeric"
                value={v.maxEntries}
                onChange={(e) => put({ maxEntries: e.target.value })}
              />
            </Field>
          </div>
          <Switch
            label="Read only: serve cached responses, but store no new ones"
            checked={v.readOnly}
            onChange={(readOnly) => put({ readOnly })}
          />
        </section>

        <section aria-labelledby="cs-semantic" className="flex flex-col gap-4 border-t border-line pt-5">
          <h3 id="cs-semantic" className="text-ui font-semibold text-fg">
            Similar prompts
          </h3>
          <Switch
            label="Also answer prompts that mean the same thing (semantic)"
            checked={v.semantic}
            onChange={(semantic) => put({ semantic })}
          />
          {v.semantic ? (
            <div className="grid gap-4 sm:grid-cols-2">
              <Field
                label="Embedding model"
                error={errors.embeddingModel}
                hint="Turns each prompt into a vector to compare."
              >
                <Select
                  aria-label="Embedding model"
                  value={v.embeddingModel}
                  onChange={(embeddingModel) => put({ embeddingModel })}
                  options={embedders}
                  placeholder="Choose a model"
                  searchable
                />
              </Field>
              <Field
                label="How similar"
                error={errors.threshold}
                hint="Between 0.80 and 1.00 (default 0.95). Higher means fewer, closer matches."
              >
                <Input
                  mono
                  inputMode="decimal"
                  value={v.threshold}
                  onChange={(e) => put({ threshold: e.target.value })}
                />
              </Field>
            </div>
          ) : null}
        </section>

        <section aria-labelledby="cs-prompt" className="flex flex-col gap-4 border-t border-line pt-5">
          <h3 id="cs-prompt" className="text-ui font-semibold text-fg">
            Provider prompt caching
          </h3>
          <Switch
            label="Mark long prompt prefixes so the provider bills them at its cached rate"
            checked={v.prompt}
            onChange={(prompt) => put({ prompt })}
          />
          {v.prompt ? (
            <Field label="Provider keeps the prefix for">
              <Segmented<'5m' | '1h'>
                label="Provider keeps the prefix for"
                value={v.promptTtl}
                onChange={(promptTtl) => put({ promptTtl })}
                options={[
                  { value: '5m', label: '5 min' },
                  { value: '1h', label: '1 h' },
                ]}
              />
            </Field>
          ) : null}
        </section>

        <section aria-labelledby="cs-advanced" className="flex flex-col gap-4 border-t border-line pt-5">
          <h3 id="cs-advanced" className="text-ui font-semibold text-fg">
            When the cache is down, and embeddings
          </h3>
          <Field label="If the cache can't be reached">
            <RadioGroup<'open' | 'closed'>
              label="If the cache can't be reached"
              value={v.failMode}
              onChange={(failMode) => put({ failMode })}
              options={[
                {
                  value: 'open',
                  label: 'Send requests to the provider',
                  description: 'The default. Apps keep working; you pay for every request until it’s back.',
                },
                {
                  value: 'closed',
                  label: 'Refuse requests',
                  description:
                    'Apps get a retryable “service unavailable” error instead of an uncached, paid answer.',
                },
              ]}
            />
          </Field>
          <Switch
            label="Cache each input of an embeddings request on its own"
            checked={v.perItem}
            onChange={(perItem) => put({ perItem })}
          />
          <p className="-mt-2 text-caption text-muted">
            Only the inputs that aren’t cached yet go to the provider, and usage counts only those.
          </p>
        </section>
      </div>
    </Drawer>
  );
}
