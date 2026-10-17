'use client';

import type { ReactNode } from 'react';

import { Badge, type Tone } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { useCan } from '@/components/ui/gate';
import { Panel } from '@/components/ui/layout';
import type { GuardrailResource } from '@/lib/api/types';
import { plural } from '@/lib/format';

import { moderationCheck } from '../posture';
import { type GuardrailType, type Method, MODERATION_TYPES, TYPE_LABEL } from '../rules';
import { CATEGORY_LABEL, useSafety } from '../store';
import type { EditorTarget } from './rule-editor';

type State = 'on' | 'partial' | 'off' | 'unknown';

const STATE: Record<State, { label: string; tone: Tone }> = {
  on: { label: 'On', tone: 'success' },
  partial: { label: 'Partly', tone: 'warn' },
  off: { label: 'Off', tone: 'neutral' },
  unknown: { label: 'Unknown', tone: 'neutral' },
};

interface Row {
  title: string;
  what: string;
  state: State;
  detail: ReactNode;
  rule: GuardrailResource | null;
  add: Method | null;
}

function on(list: GuardrailResource[], types: GuardrailType[]): GuardrailResource[] {
  return list.filter((g) => g.spec.enabled !== false && types.includes(g.spec.type));
}

function names(list: GuardrailResource[]): string {
  const shown = list.slice(0, 3).map((g) => g.name);
  return `${shown.join(', ')}${list.length > 3 ? ` and ${list.length - 3} more` : ''}`;
}

/** Which detections are set up and where: each kind the console tracks, mapped to the rules that do it. */
export function CoveragePanel({ onEdit }: { onEdit: (t: EditorTarget) => void }) {
  const guardrails = useSafety((s) => s.guardrails.data) ?? [];
  const policies = useSafety((s) => s.policies.data);
  const teams = useSafety((s) => s.teams.data);
  const models = useSafety((s) => s.models.data);
  const canCreate = useCan('createGuardrail');

  const moderation = on(guardrails, MODERATION_TYPES);
  const output =
    policies && models
      ? moderationCheck({
          policies: policies.spec,
          models: Object.fromEntries(models.map((m) => [m.name, m.spec])),
          guardrails: { definitions: Object.fromEntries(guardrails.map((g) => [g.name, g.spec])) },
        })
      : null;

  const overriding = (teams ?? []).filter((t) => {
    const g = t.spec.policies?.guardrails;
    return (g?.input?.length ?? 0) + (g?.output?.length ?? 0) + (g?.exclude?.length ?? 0) > 0;
  });
  const excluding = overriding.filter((t) => (t.spec.policies?.guardrails?.exclude?.length ?? 0) > 0);

  const secrets = on(guardrails, ['secrets']);
  const blocking = secrets.filter((g) => (g.spec.action ?? 'block') === 'block');

  const partners = on(guardrails, [
    'classifier',
    'llm_judge',
    'bedrock_guardrails',
    'azure_content_safety',
    'model_armor',
    'lakera',
  ]);
  const partnerCategories = [
    ...new Set(
      partners.flatMap((g) => [
        ...Object.values(g.spec.categories ?? {}),
        ...Object.keys(g.spec.classifier?.thresholds ?? {}),
      ]),
    ),
  ];
  const webhooks = on(guardrails, ['webhook']);

  const rows: Row[] = [
    {
      title: 'Output moderation',
      what: 'Harmful answers are caught before they reach your apps.',
      state: output
        ? output.pass
          ? 'on'
          : moderation.length > 0
            ? 'partial'
            : 'off'
        : moderation.length > 0
          ? 'unknown'
          : 'off',
      detail: output ? output.detail : moderation.length > 0 ? names(moderation) : 'No moderation rule.',
      rule: moderation[0] ?? null,
      add: 'classifier',
    },
    {
      title: 'Team overrides and exceptions',
      what: 'Teams that add their own guardrails or turn one off for themselves.',
      state: teams ? (overriding.length > 0 ? 'on' : 'off') : 'unknown',
      detail: teams
        ? overriding.length > 0
          ? `${plural(overriding.length, 'team')} (${overriding
              .slice(0, 3)
              .map((t) => t.name)
              .join(
                ', ',
              )})${excluding.length > 0 ? `, ${plural(excluding.length, 'team')} turning one off` : ''}.`
          : 'Every team uses the organization’s guardrails as they are.'
        : 'Your role can’t read teams.',
      rule: null,
      add: null,
    },
    {
      title: 'Secrets and exposed keys',
      what: 'Keys and tokens in prompts are stopped; a leaked Antirion key is flagged for its owner to rotate.',
      state: secrets.length === 0 ? 'off' : blocking.length > 0 ? 'on' : 'partial',
      detail:
        secrets.length === 0
          ? 'No secrets rule.'
          : blocking.length > 0
            ? `${names(blocking)} ${blocking.length === 1 ? 'blocks' : 'block'}; leaked keys are flagged.`
            : `${names(secrets)} ${secrets.length === 1 ? 'doesn’t block' : 'don’t block'}; leaked keys are still flagged.`,
      rule: blocking[0] ?? secrets[0] ?? null,
      add: 'secrets',
    },
    {
      title: 'Classifier and partner detections',
      what: 'Medical, financial, prompt injection and jailbreak content, scored by a classifier or a partner service.',
      state: partners.length > 0 ? 'on' : 'off',
      detail:
        partners.length > 0 ? (
          <>
            {partners.map((g) => `${g.name} (${TYPE_LABEL[g.spec.type]})`).join(', ')}
            {partnerCategories.length > 0
              ? ` · ${partnerCategories.map((c) => CATEGORY_LABEL[c] ?? c).join(', ')}`
              : ''}
          </>
        ) : (
          'No classifier or partner rule.'
        ),
      rule: partners[0] ?? null,
      add: 'classifier',
    },
    {
      title: 'Names, addresses, code and copyright',
      what: 'Checks your own service runs through a webhook: named-entity recognition, code fingerprints, copyright matching.',
      state: webhooks.length > 0 ? 'on' : 'off',
      detail: webhooks.length > 0 ? names(webhooks) : 'No webhook rule.',
      rule: webhooks[0] ?? null,
      add: 'webhook',
    },
  ];

  return (
    <Panel
      title="Detection coverage"
      subtitle="What is checked, and by which rules"
      bodyClassName="px-0 pb-0"
    >
      <ul className="flex flex-col divide-y divide-line border-t border-line" aria-label="Detection coverage">
        {rows.map((r) => (
          <li key={r.title} className="flex flex-wrap items-start gap-x-4 gap-y-2 px-5 py-3.5">
            <div className="min-w-0 flex-1">
              <p className="flex items-center gap-2 text-ui font-medium text-fg">
                {r.title}
                <Badge tone={STATE[r.state].tone} dot>
                  {STATE[r.state].label}
                </Badge>
              </p>
              <p className="mt-0.5 text-caption text-muted">{r.what}</p>
              <p className="mt-1 text-caption text-fg">{r.detail}</p>
            </div>
            {r.rule ? (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => r.rule && onEdit({ kind: 'edit', name: r.rule.name })}
              >
                Open rule
              </Button>
            ) : r.add && canCreate ? (
              <Button size="sm" icon="plus" onClick={() => r.add && onEdit({ kind: 'new', method: r.add })}>
                Add rule
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
    </Panel>
  );
}
