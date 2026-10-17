'use client';

import { useEffect, useMemo, useState, type ReactNode } from 'react';

import { Badge, type Tone } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Checkbox, Combobox, Field, Input, Segmented, Select, Switch, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { Banner, InlineError } from '@/components/ui/states';
import { modelOption } from '@/components/ui/marks';
import type { GuardrailResource } from '@/lib/api/types';
import { useEdited } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import { type GuardrailTestResult, safetyApi } from '../api';
import {
  actionsFor,
  CATEGORIES,
  type DraftErrors,
  draftOf,
  emptyDraft,
  ENTITY_NAME,
  impliedCategory,
  matchWord,
  type Method,
  METHODS,
  PII_ENTITIES,
  PII_PRESERVE,
  PRESERVE_NAME,
  type RuleDraft,
  SECRET_DETECTORS,
  specOf,
  stagesFor,
  TRANSFORMS,
  TYPE_LABEL,
} from '../rules';
import { CATEGORY_LABEL, useSafety } from '../store';

/** What the editor opens on: a new rule (of a method), or a guardrail, maybe with a prepared change. */
export type EditorTarget =
  | { kind: 'new'; method?: Method }
  | { kind: 'edit'; name: string; prepared?: { draft: RuleDraft; change: string } };

const ACTION_LABEL: Record<string, string> = {
  block: 'Block',
  redact: 'Redact',
  flag: 'Flag only',
  divert: 'Divert',
};

const ACTION_HINT: Record<string, string> = {
  block: 'The request is refused; nothing reaches the provider.',
  redact: 'Matches are replaced before the provider sees them.',
  flag: 'The request goes through as is and is recorded for review.',
  divert: 'Another Model answers instead, such as a safe template Model.',
};

/** Where a guardrail runs at the gateway level, from the gateway's policies. */
export function useAttachment(name: string | null): { input: boolean; output: boolean } {
  const lists = useSafety((s) => s.policies.data?.spec.guardrails);
  return {
    input: name !== null && (lists?.input ?? []).includes(name),
    output: name !== null && (lists?.output ?? []).includes(name),
  };
}

function Lines({
  label,
  hint,
  error,
  value,
  onChange,
  placeholder,
  mono,
  rows = 4,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | undefined;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  mono?: boolean;
  rows?: number;
}) {
  return (
    <Field label={label} hint={hint} error={error}>
      <Textarea
        mono={mono}
        rows={rows}
        spellCheck={false}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
    </Field>
  );
}

function CheckGrid<T extends string>({
  legend,
  all,
  value,
  onChange,
  name,
  error,
}: {
  legend: string;
  all: readonly T[];
  value: T[];
  onChange: (v: T[]) => void;
  name: (v: T) => string;
  error?: string | undefined;
}) {
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-1 text-ui font-medium text-fg">{legend}</legend>
      <div className="grid grid-cols-1 gap-x-4 gap-y-1.5 sm:grid-cols-2">
        {all.map((v) => (
          <Checkbox
            key={v}
            label={name(v)}
            checked={value.includes(v)}
            onChange={(on) => onChange(on ? [...value, v] : value.filter((x) => x !== v))}
          />
        ))}
      </div>
      {error ? (
        <p role="alert" className="text-caption text-error">
          {error}
        </p>
      ) : null}
    </fieldset>
  );
}

function ModelPicker({
  label,
  hint,
  value,
  onChange,
  error,
}: {
  label: string;
  hint?: string;
  value: string;
  onChange: (v: string) => void;
  error?: string | undefined;
}) {
  const models = useSafety((s) => s.models.data);
  const options = (models ?? []).map((m) => modelOption(m));
  if (value && !options.some((o) => o.value === value)) options.unshift(modelOption(value));
  return (
    <Field label={label} hint={hint} error={error}>
      {models ? (
        <Combobox value={value} onChange={onChange} placeholder="Choose a Model" options={options} />
      ) : (
        <Input mono value={value} onChange={(e) => onChange(e.target.value)} placeholder="Model name" />
      )}
    </Field>
  );
}

/** The settings of the chosen method. */
function MethodFields({
  draft,
  set,
  errors,
}: {
  draft: RuleDraft;
  set: (patch: Partial<RuleDraft>) => void;
  errors: DraftErrors;
}) {
  const replacement =
    draft.action === 'redact' &&
    ['regex', 'allowlist', 'keywords', 'pii', 'secrets', 'webhook'].includes(draft.method ?? '') ? (
      <Field
        label="Replace matches with"
        hint="Labels read like [EMAIL]; masks hide the characters; hashes stay comparable."
      >
        <Segmented<RuleDraft['replacement']>
          label="Replace matches with"
          value={draft.replacement}
          onChange={(replacement) => set({ replacement })}
          options={[
            { value: 'label', label: 'Label' },
            { value: 'mask', label: 'Mask' },
            { value: 'hash', label: 'Hash' },
          ]}
        />
      </Field>
    ) : null;
  switch (draft.method) {
    case 'regex':
    case 'allowlist':
      return (
        <>
          <Lines
            label={draft.method === 'allowlist' ? 'Allowed patterns' : 'Patterns'}
            hint={
              draft.method === 'allowlist'
                ? 'One regular expression per line. Text that matches none of them is a finding.'
                : 'One regular expression per line. Any match is a finding.'
            }
            mono
            error={errors.patterns}
            value={draft.patterns}
            onChange={(patterns) => set({ patterns })}
            placeholder={'\\b[A-Z]{2}\\d{2}[A-Z0-9]{11,30}\\b'}
          />
          {draft.method === 'regex' ? (
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <Field label="Checksum" hint="Only matches whose digits pass the check count.">
                <Select
                  value={draft.validate}
                  onChange={(v) => set({ validate: v === 'luhn' || v === 'iban_mod97' ? v : 'none' })}
                  options={[
                    { value: 'none', label: 'None' },
                    { value: 'luhn', label: 'Luhn (card numbers)' },
                    { value: 'iban_mod97', label: 'IBAN mod-97' },
                  ]}
                />
              </Field>
              <Field label="Label" optional hint="What a redacted match becomes, such as IBAN.">
                <Input
                  mono
                  value={draft.label}
                  onChange={(e) => set({ label: e.target.value })}
                  placeholder="REDACTED"
                />
              </Field>
            </div>
          ) : null}
          {replacement}
        </>
      );
    case 'keywords':
      return (
        <>
          <Lines
            label="Words and phrases"
            hint="One per line."
            error={errors.words}
            value={draft.words}
            onChange={(words) => set({ words })}
            placeholder={'ignore previous instructions\ndeveloper mode'}
          />
          <Field label="Match">
            <Segmented<RuleDraft['match']>
              label="Match"
              value={draft.match}
              onChange={(match) => set({ match })}
              options={[
                { value: 'word', label: 'Whole words' },
                { value: 'substring', label: 'Anywhere in the text' },
              ]}
            />
          </Field>
          {replacement}
        </>
      );
    case 'pii':
      return (
        <>
          <CheckGrid
            legend="Look for"
            all={PII_ENTITIES}
            value={draft.entities}
            onChange={(entities) => set({ entities })}
            name={(e) => ENTITY_NAME[e] ?? e}
            error={errors.entities}
          />
          <CheckGrid
            legend="Keep readable"
            all={PII_PRESERVE}
            value={draft.preserve}
            onChange={(preserve) => set({ preserve, ...(preserve.length > 0 ? { reversible: false } : {}) })}
            name={(p) => PRESERVE_NAME[p]}
          />
          {replacement}
          {draft.action === 'redact' ? (
            <Checkbox
              label="Put the values back in the answer"
              description="The provider sees numbered labels such as [EMAIL_1]; the answer gets the real values back. Needs labels and nothing kept readable."
              checked={draft.reversible}
              disabled={draft.replacement !== 'label' || draft.preserve.length > 0}
              onChange={(reversible) => set({ reversible })}
            />
          ) : null}
          {errors.reversible ? (
            <p role="alert" className="text-caption text-error">
              {errors.reversible}
            </p>
          ) : null}
        </>
      );
    case 'secrets':
      return (
        <>
          <CheckGrid
            legend="Look for"
            all={SECRET_DETECTORS}
            value={draft.detectors}
            onChange={(detectors) => set({ detectors })}
            name={(d) => ENTITY_NAME[d] ?? d}
            error={errors.detectors}
          />
          <Field
            label="Minimum entropy"
            optional
            error={errors.minEntropy}
            hint="Bits per character, 0 to 8. Matches that look less random than this are ignored."
          >
            <Input
              inputMode="decimal"
              value={draft.minEntropy}
              onChange={(e) => set({ minEntropy: e.target.value })}
              placeholder="3.5"
            />
          </Field>
          {replacement}
        </>
      );
    case 'classifier':
    case 'webhook':
      return (
        <>
          <Field
            label={draft.method === 'classifier' ? 'Endpoint' : 'Service address'}
            error={errors.url}
            hint={
              draft.method === 'classifier'
                ? 'It receives the text and answers a score per label.'
                : 'It receives the text and answers a verdict, with a category when it found something.'
            }
          >
            <Input
              mono
              value={draft.url}
              onChange={(e) => set({ url: e.target.value })}
              placeholder="https://"
            />
          </Field>
          {draft.method === 'classifier' ? (
            <Lines
              label="Thresholds"
              hint="One label = score per line; a score at or above it is a finding."
              mono
              rows={3}
              error={errors.thresholds}
              value={draft.thresholds}
              onChange={(thresholds) => set({ thresholds })}
              placeholder={'jailbreak = 0.9\nmedical = 0.8'}
            />
          ) : (
            <Field label="Send">
              <Segmented<RuleDraft['webhookMode']>
                label="Send"
                value={draft.webhookMode}
                onChange={(webhookMode) => set({ webhookMode })}
                options={[
                  { value: 'segments', label: 'The text only' },
                  { value: 'body', label: 'The whole request' },
                ]}
              />
            </Field>
          )}
          <Lines
            label="Report labels as"
            hint={`One label = category per line. Categories: ${CATEGORIES.filter((c) => c !== 'uninspectable').join(', ')}. Anything else is reported as custom.`}
            mono
            rows={3}
            error={errors.categories}
            value={draft.categories}
            onChange={(categories) => set({ categories })}
            placeholder={'jailbreak = prompt_injection\nmedical = pii'}
          />
          {replacement}
        </>
      );
    case 'topic':
      return (
        <>
          <ModelPicker
            label="Embeddings Model"
            hint="Compares each request with the topics."
            value={draft.model}
            onChange={(model) => set({ model })}
            error={errors.model}
          />
          <Lines
            label="Allowed topics"
            hint="One description per line. A request close to none of them is off topic."
            error={errors.allowedTopics}
            value={draft.allowedTopics}
            onChange={(allowedTopics) => set({ allowedTopics })}
            placeholder="Questions about our product and billing"
          />
          <Lines
            label="Denied topics"
            hint="One description per line. A request close to one of them is off topic."
            value={draft.deniedTopics}
            onChange={(deniedTopics) => set({ deniedTopics })}
            placeholder="Medical or legal advice"
          />
          <Field
            label="Similarity threshold"
            error={errors.threshold}
            hint="From 0 to 1; higher needs a closer match."
          >
            <Input
              inputMode="decimal"
              value={draft.threshold}
              onChange={(e) => set({ threshold: e.target.value })}
            />
          </Field>
        </>
      );
    case 'llm_rewrite':
      return (
        <>
          <ModelPicker
            label="Model"
            value={draft.model}
            onChange={(model) => set({ model })}
            error={errors.model}
          />
          <Lines
            label="Instructions"
            hint="What the Model should change, such as removing names or softening the tone."
            error={errors.prompt}
            value={draft.prompt}
            onChange={(prompt) => set({ prompt })}
          />
        </>
      );
    case 'json_schema':
      return (
        <>
          <Checkbox
            label="Use the schema the request asks for"
            description="Answers are checked against the response format each request names."
            checked={draft.useRequestSchema}
            onChange={(useRequestSchema) => set({ useRequestSchema })}
          />
          {draft.useRequestSchema ? null : (
            <Lines
              label="Schema"
              mono
              rows={6}
              error={errors.schema}
              value={draft.schema}
              onChange={(schema) => set({ schema })}
              placeholder={'{\n  "type": "object",\n  "required": ["answer"]\n}'}
            />
          )}
          <Field
            label="Ask again"
            error={errors.reask}
            hint="Times an answer that fails is asked for again, 0 to 5."
          >
            <Input inputMode="numeric" value={draft.reask} onChange={(e) => set({ reask: e.target.value })} />
          </Field>
        </>
      );
    case 'json_repair':
      return (
        <p className="text-ui text-muted">
          Answers that are almost valid JSON (a trailing comma, a missing quote or bracket) are fixed before
          they reach your app. There is nothing to set.
        </p>
      );
    case 'template':
      return (
        <>
          <Lines
            label="Templates"
            hint="A JSON object: each name maps to its messages, with {{variable}} placeholders the request fills."
            mono
            rows={8}
            error={errors.templates}
            value={draft.templates}
            onChange={(templates) => set({ templates })}
            placeholder={
              '{\n  "support_reply": {\n    "messages": [\n      { "role": "system", "content": "You help {{customer}}." }\n    ]\n  }\n}'
            }
          />
          <Checkbox
            label="Only templated requests"
            description="Requests that don't name a template are refused."
            checked={draft.templateRequired}
            onChange={(templateRequired) => set({ templateRequired })}
          />
        </>
      );
    case 'decorator':
      return (
        <>
          <Lines
            label="Prefix"
            hint="Added before the last user message."
            error={errors.userPrefix}
            rows={2}
            value={draft.userPrefix}
            onChange={(userPrefix) => set({ userPrefix })}
          />
          <Lines
            label="Suffix"
            hint="Added after it."
            rows={2}
            value={draft.userSuffix}
            onChange={(userSuffix) => set({ userSuffix })}
          />
        </>
      );
    case null:
      return (
        <p className="text-ui text-muted">
          This method ({TYPE_LABEL[draft.type]}) is set in the configuration. Here you can turn it on or off
          and change its action, severity, description and where it runs.
        </p>
      );
  }
}

const VERDICT_TONE: Record<string, Tone> = { block: 'error', redact: 'warn', flag: 'info', pass: 'success' };

/** The text with the matched spans marked (code-point offsets). */
function Marked({ text, spans }: { text: string; spans: GuardrailTestResult['spans'] }) {
  const chars = Array.from(text);
  const parts: ReactNode[] = [];
  let at = 0;
  for (const [i, s] of [...spans].sort((a, b) => a.start - b.start).entries()) {
    if (s.start < at) continue;
    parts.push(chars.slice(at, s.start).join(''));
    parts.push(
      <mark
        key={i}
        className="rounded-xs bg-[color-mix(in_oklab,var(--ant-warn)_30%,transparent)] px-0.5 text-fg"
      >
        {chars.slice(s.start, s.end).join('')}
        <span className="sr-only"> ({matchWord(s.entity)})</span>
      </mark>,
    );
    at = s.end;
  }
  parts.push(chars.slice(at).join(''));
  return <p className="text-ui leading-relaxed break-words whitespace-pre-wrap text-fg">{parts}</p>;
}

/** Runs a saved guardrail on sample text, without sending anything to a provider. */
function TestPanel({ name }: { name: string }) {
  const [stage, setStage] = useState<'input' | 'output'>('input');
  const [text, setText] = useState('');
  const [result, setResult] = useState<{ text: string; r: GuardrailTestResult } | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const canTest = useCan('testGuardrail');
  if (!canTest) return null;
  return (
    <section
      aria-label="Test this rule"
      className="flex flex-col gap-3 rounded-sm border border-line bg-sunken p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-ui font-semibold text-fg">Test this rule</h3>
        <Segmented<'input' | 'output'>
          label="Test as"
          value={stage}
          onChange={setStage}
          options={[
            { value: 'input', label: 'A prompt' },
            { value: 'output', label: 'An answer' },
          ]}
        />
      </div>
      <Field
        label="Sample text"
        hint="Uses the saved rule; save your changes first to test them. Nothing is sent to a provider or logged."
      >
        <Textarea
          rows={3}
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="Contact me at jane@example.com"
        />
      </Field>
      <div className="flex items-center gap-3">
        <Button
          size="sm"
          icon="play"
          busy={busy}
          disabled={!text.trim()}
          disabledReason="Enter some text to test"
          onClick={() => {
            setBusy(true);
            setError(null);
            safetyApi
              .test(name, stage, text)
              .then((r) => setResult({ text, r }))
              .catch((e: unknown) => setError(e))
              .finally(() => setBusy(false));
          }}
        >
          Run test
        </Button>
        <InlineError error={error} />
      </div>
      {result ? (
        <div className="flex flex-col gap-2" role="status">
          <p className="flex flex-wrap items-center gap-2 text-ui text-muted">
            <Badge tone={VERDICT_TONE[result.r.verdict] ?? 'neutral'} dot>
              {result.r.verdict === 'pass' ? 'Passes' : (ACTION_LABEL[result.r.verdict] ?? result.r.verdict)}
            </Badge>
            {result.r.category ? <span>{CATEGORY_LABEL[result.r.category] ?? result.r.category}</span> : null}
            <span>· {result.r.spans.length === 1 ? '1 match' : `${result.r.spans.length} matches`}</span>
          </p>
          {result.r.spans.length > 0 ? <Marked text={result.text} spans={result.r.spans} /> : null}
          {typeof result.r.redacted === 'string' && result.r.redacted !== result.text ? (
            <div>
              <p className="text-caption text-muted">What goes on</p>
              <p className="mono text-ui break-words whitespace-pre-wrap text-fg">{result.r.redacted}</p>
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

function Editor({
  target,
  onClose,
  onEdited,
}: {
  target: EditorTarget;
  onClose: () => void;
  onEdited: (edited: boolean) => void;
}) {
  const guardrails = useSafety((s) => s.guardrails.data);
  const policies = useSafety((s) => s.policies);
  const saveRule = useSafety((s) => s.saveRule);
  const deleteRule = useSafety((s) => s.deleteRule);
  const canWrite = useCan('replaceGuardrail');
  const canAttach = useCan('patchPoliciesSettings') && policies.data !== null;
  const original: GuardrailResource | null =
    target.kind === 'edit' ? (guardrails?.find((g) => g.name === target.name) ?? null) : null;
  const attached = useAttachment(original?.name ?? null);
  const initial = useMemo(
    () =>
      target.kind === 'edit'
        ? (target.prepared?.draft ?? (original ? draftOf(original.name, original.spec, attached) : null))
        : emptyDraft(target.method ?? 'regex'),
    // The form starts from what was open when the drawer opened; later reads don't reset it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );
  const [draft, setDraft] = useState<RuleDraft | null>(initial);
  const [errors, setErrors] = useState<DraftErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const edited = useEdited(draft);
  useEffect(() => onEdited(edited), [edited, onEdited]);
  if (!draft) return <p className="text-ui text-muted">This rule no longer exists.</p>;
  const set = (patch: Partial<RuleDraft>) => {
    setDraft({ ...draft, ...patch });
    setErrors({});
  };
  const transform = TRANSFORMS.includes(draft.type);
  const actions = actionsFor(draft.method, draft.type);
  const stages = stagesFor(draft.type, draft.action, original?.spec.execution);
  const category = impliedCategory({
    type: draft.type,
    regex: draft.method === 'allowlist' ? { allow: ['x'] } : {},
  });

  async function save() {
    if (!draft) return;
    const result = specOf(draft, original?.spec ?? null);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveRule({
        name: draft.name,
        spec: result.spec,
        original,
        attach: canAttach
          ? { input: draft.onInput && stages.input, output: draft.onOutput && stages.output }
          : null,
      });
      notify('success', original ? 'Rule saved' : 'Rule created', draft.name);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <form
        id="rule-editor"
        noValidate
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        {target.kind === 'edit' && target.prepared ? (
          <Banner tone="info" title="A change is prepared">
            {target.prepared.change} Review it, then save. Past events stay as they were.
          </Banner>
        ) : null}
        {target.kind === 'new' ? (
          <Field label="Method" hint={METHODS.find((m) => m.value === draft.method)?.description}>
            <Select
              value={draft.method ?? 'regex'}
              onChange={(v) => {
                const method = METHODS.find((m) => m.value === v)?.value ?? 'regex';
                setDraft({
                  ...emptyDraft(method),
                  name: draft.name,
                  description: draft.description,
                  severity: draft.severity,
                });
                setErrors({});
              }}
              options={METHODS.map((m) => ({ value: m.value, label: m.label }))}
            />
          </Field>
        ) : null}
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field
            label="Name"
            error={errors.name}
            hint={
              target.kind === 'new'
                ? 'Lowercase letters, digits and underscores; it can’t be changed later.'
                : undefined
            }
          >
            <Input
              mono
              value={draft.name}
              readOnly={target.kind === 'edit'}
              onChange={(e) => set({ name: e.target.value })}
              placeholder="iban_numbers"
              data-autofocus={target.kind === 'new' ? true : undefined}
            />
          </Field>
          <Field label="Reported as" hint="The category events of this rule are filed under.">
            <Input
              readOnly
              value={
                category
                  ? (CATEGORY_LABEL[category] ?? category)
                  : transform
                    ? 'Changes requests; reports nothing'
                    : 'The category each label maps to'
              }
            />
          </Field>
        </div>
        <MethodFields draft={draft} set={set} errors={errors} />
        {transform ? null : (
          <div className="flex flex-col gap-4">
            {actions.length > 1 ? (
              <Field label="Action" hint={draft.action ? ACTION_HINT[draft.action] : undefined}>
                <Segmented<string>
                  label="Action"
                  value={draft.action}
                  onChange={(v) => set({ action: actions.find((a) => a === v) ?? draft.action })}
                  options={actions.map((a) => ({ value: a, label: ACTION_LABEL[a] ?? a }))}
                />
              </Field>
            ) : null}
            {draft.action === 'divert' ? (
              <ModelPicker
                label="Answer with"
                hint="The Model whose route serves a request this rule catches."
                value={draft.divertTo}
                onChange={(divertTo) => set({ divertTo })}
                error={errors.divertTo}
              />
            ) : null}
            <Field
              label="Severity"
              hint="Without one, it follows the action: block is high, redact medium, flag and divert low."
            >
              <Select
                value={draft.severity}
                onChange={(v) =>
                  set({
                    severity: v === 'critical' || v === 'high' || v === 'medium' || v === 'low' ? v : '',
                  })
                }
                emptyOption="By action"
                options={[
                  { value: 'critical', label: 'Critical' },
                  { value: 'high', label: 'High' },
                  { value: 'medium', label: 'Medium' },
                  { value: 'low', label: 'Low' },
                ]}
              />
            </Field>
          </div>
        )}
        <Field label="Description" optional hint="A note for other admins: why this rule exists.">
          <Textarea
            rows={2}
            value={draft.description}
            onChange={(e) => set({ description: e.target.value })}
          />
        </Field>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-ui font-medium text-fg">Where it runs</legend>
          {canAttach ? (
            <>
              <Checkbox
                label="Every prompt"
                description={
                  stages.input
                    ? 'At the gateway level, for all organizations.'
                    : 'This method reads answers only.'
                }
                checked={draft.onInput && stages.input}
                disabled={!stages.input}
                onChange={(onInput) => set({ onInput })}
              />
              <Checkbox
                label="Every answer"
                description={
                  stages.output
                    ? 'Answers are checked before they reach your apps.'
                    : draft.action === 'divert'
                      ? 'A diverted request is decided on its prompt only.'
                      : 'This method reads prompts only.'
                }
                checked={draft.onOutput && stages.output}
                disabled={!stages.output}
                onChange={(onOutput) => set({ onOutput })}
              />
            </>
          ) : (
            <p className="text-caption text-muted">
              Your role can&apos;t change where guardrails run; an organization, team or Model policy can name
              this rule.
            </p>
          )}
          {canAttach && !(draft.onInput && stages.input) && !(draft.onOutput && stages.output) ? (
            <p className="text-caption text-warn">
              It checks nothing until a policy names it: here, or in an organization, team or Model.
            </p>
          ) : null}
        </fieldset>
        <Switch label="On" checked={draft.enabled} onChange={(enabled) => set({ enabled })} />
        <InlineError error={error} />
      </form>
      {original ? (
        <div className="mt-6">
          <TestPanel name={original.name} />
        </div>
      ) : null}
      <div className="mt-6 flex flex-wrap items-center justify-end gap-2 border-t border-line pt-4">
        {original && canWrite ? (
          <Button variant="danger" icon="trash" className="mr-auto" onClick={() => setDeleting(true)}>
            Delete rule
          </Button>
        ) : null}
        <CancelButton />
        <Button
          type="submit"
          form="rule-editor"
          variant="primary"
          busy={busy}
          disabled={!canWrite}
          disabledReason="Your role can't change guardrails"
        >
          {original ? 'Save rule' : 'Create rule'}
        </Button>
      </div>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title={`Delete rule ${original?.name ?? ''}?`}
        objectName={original?.name}
        effect="Requests stop being checked by it, here and in every policy that names it; its past events stay."
        typeToConfirm={original?.name}
        confirmLabel="Delete rule"
        onConfirm={async () => {
          if (!original) return;
          await deleteRule(original.name);
          notify('success', 'Rule deleted', original.name);
          onClose();
        }}
      />
    </>
  );
}

/** The rule drawer: create, edit, test and delete a guardrail. */
export function RuleEditor({ target, onClose }: { target: EditorTarget | null; onClose: () => void }) {
  const [edited, setEdited] = useState(false);
  return (
    <Drawer
      open={target !== null}
      dirty={edited}
      onClose={() => {
        setEdited(false);
        onClose();
      }}
      width="lg"
      title={target === null ? '' : target.kind === 'new' ? 'New rule' : target.name}
      subtitle={
        target?.kind === 'new'
          ? 'A guardrail that checks or changes requests and answers'
          : 'Edit, test or delete this rule'
      }
    >
      {target ? (
        <Editor
          key={JSON.stringify(target)}
          target={target}
          onClose={() => {
            setEdited(false);
            onClose();
          }}
          onEdited={setEdited}
        />
      ) : null}
    </Drawer>
  );
}
