'use client';

import { useState } from 'react';

import { Badge } from '@/components/ui/badge';

import { ConfirmDialog } from '@/components/ui/confirm';
import { Segmented } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Panel } from '@/components/ui/layout';
import { EmptyAction, EmptyState } from '@/components/ui/states';
import type { GuardrailResource } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount } from '@/lib/format';
import { notify } from '@/stores/ui';

import {
  applyStrictness,
  detectionMethod,
  ENTITY_NAME,
  maskedSample,
  PII_ENTITIES,
  SECRET_DETECTORS,
  type Strictness,
  STRICTNESS,
  strictnessChanges,
  strictnessOf,
} from '../rules';
import { useSafety } from '../store';
import type { EditorTarget } from './rule-editor';

/** The gateway's personal-data guardrail: the first one on, run on every prompt at the gateway level. */
export function useGatewayPii(): GuardrailResource | null {
  const guardrails = useSafety((s) => s.guardrails.data);
  const input = useSafety((s) => s.policies.data?.spec.guardrails?.input);
  return (
    (guardrails ?? []).find(
      (g) => g.spec.type === 'pii' && g.spec.enabled !== false && (input ?? []).includes(g.name),
    ) ??
    (guardrails ?? []).find((g) => g.spec.type === 'pii' && g.spec.enabled !== false) ??
    null
  );
}

function Strictness({ rule, onEdit }: { rule: GuardrailResource | null; onEdit: (t: EditorTarget) => void }) {
  const updateRule = useSafety((s) => s.updateRule);
  const canWrite = useCan('replaceGuardrail');
  const [pending, setPending] = useState<Strictness | null>(null);
  if (!rule)
    return (
      <EmptyState
        title="No personal data rule yet"
        description="It redacts emails, phone and card numbers before a provider sees them."
        action={
          canWrite ? (
            <EmptyAction onClick={() => onEdit({ kind: 'new', method: 'pii' })}>
              Create a personal data rule
            </EmptyAction>
          ) : undefined
        }
      />
    );
  const current = strictnessOf(rule.spec);
  const changes = pending ? strictnessChanges(rule.spec, pending) : [];
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-3">
        <Segmented<string>
          label="Redaction strictness"
          value={current ?? 'custom'}
          onChange={(v) => {
            if (v === 'loose' || v === 'balanced' || v === 'strict') setPending(v);
          }}
          options={[
            ...(['loose', 'balanced', 'strict'] as const).map((k) => ({
              value: k,
              label: STRICTNESS[k].label,
            })),
            ...(current ? [] : [{ value: 'custom', label: 'Custom' }]),
          ]}
        />
        <span className="text-caption text-muted">
          Applies to <span className="mono text-fg">{rule.name}</span>
        </span>
      </div>
      <p className="text-caption text-muted">
        {current
          ? STRICTNESS[current].summary
          : 'The rule’s settings match no preset. Choosing one replaces what it looks for, keeps and how it replaces.'}
      </p>
      <ConfirmDialog
        open={pending !== null}
        onClose={() => setPending(null)}
        tone="primary"
        withReason={false}
        title={pending ? `Make redaction ${STRICTNESS[pending].label.toLowerCase()}?` : ''}
        objectName={rule.name}
        effect={
          changes.length === 0 ? (
            'Nothing changes: the rule already matches this preset.'
          ) : (
            <ul className="flex list-disc flex-col gap-1 pl-5">
              {changes.map((c) => (
                <li key={c}>{c}.</li>
              ))}
            </ul>
          )
        }
        confirmLabel={pending ? `Make it ${STRICTNESS[pending].label.toLowerCase()}` : 'Apply'}
        onConfirm={async () => {
          if (!pending) return;
          await updateRule(rule, applyStrictness(rule.spec, pending));
          notify('success', `Redaction is ${STRICTNESS[pending].label.toLowerCase()}`, rule.name);
        }}
      />
    </div>
  );
}

/**
 * Each kind of personal data and secret with its matches over 24 hours, how a value looks after
 * redaction, how it is found and whether a rule checks it; a card filters the events to it.
 */
export function PiiPanel({
  onFilter,
  onEdit,
}: {
  onFilter: (entity: string) => void;
  onEdit: (t: EditorTarget) => void;
}) {
  const byEntity = useSafety((s) => s.byEntity.data);
  const guardrails = useSafety((s) => s.guardrails.data);
  const rule = useGatewayPii();
  const secrets =
    (guardrails ?? []).find((g) => g.spec.type === 'secrets' && g.spec.enabled !== false) ?? null;
  const pii = rule?.spec.pii;
  const replacement = rule?.spec.replacement ?? 'label';
  const preserve = pii?.preserve ?? [];
  const cards = [
    ...PII_ENTITIES.map((e) => ({
      id: e,
      kind: 'pii' as const,
      on: Boolean(rule) && (pii?.entities ?? PII_ENTITIES).includes(e),
      masked: maskedSample(e, replacement, preserve),
      method: detectionMethod(e),
    })),
    ...SECRET_DETECTORS.map((d) => ({
      id: d,
      kind: 'secret' as const,
      on: Boolean(secrets) && (secrets?.spec.secrets?.detectors ?? SECRET_DETECTORS).includes(d),
      masked: secrets?.spec.action === 'redact' ? `[${d.toUpperCase()}]` : 'Request blocked',
      method: detectionMethod(d, secrets?.spec.secrets?.min_entropy),
    })),
  ];
  return (
    <div className="flex flex-col">
      <Panel title="Redaction strictness" subtitle="Presets over the gateway's personal data rule">
        <Strictness rule={rule} onEdit={onEdit} />
      </Panel>
      <Panel
        title="Personal data and secrets"
        subtitle="Matches in the last 24 hours · choose a card to see its events"
        bodyClassName="pb-5"
      >
        <ul
          className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4"
          aria-label="Kinds of personal data and secrets"
        >
          {cards.map((c) => {
            const matches = byEntity?.[c.id] ?? 0;
            return (
              <li key={c.id}>
                <button
                  type="button"
                  onClick={() => onFilter(c.id)}
                  className={cn(
                    'flex h-full w-full flex-col gap-2 rounded-sm border border-line bg-raised px-3.5 py-3 text-left hover:border-line-strong hover:bg-hover',
                    !c.on && 'opacity-70',
                  )}
                >
                  <span className="flex items-start justify-between gap-2">
                    <span className="text-ui font-medium text-fg">{ENTITY_NAME[c.id] ?? c.id}</span>
                    <Badge tone={c.on ? 'success' : 'neutral'}>{c.on ? 'Checked' : 'Not checked'}</Badge>
                  </span>
                  <span className="flex items-baseline gap-1.5">
                    <span className="mono text-title font-semibold text-fg tabular-nums">
                      {byEntity && (c.on || matches > 0) ? formatCount(matches) : '—'}
                    </span>
                    <span className="text-caption text-muted">matches, 24 h</span>
                  </span>
                  <span className="mono truncate text-caption text-fg">{c.on ? c.masked : '—'}</span>
                  <span className="text-caption text-muted">{c.method}</span>
                </button>
              </li>
            );
          })}
        </ul>
      </Panel>
    </div>
  );
}
