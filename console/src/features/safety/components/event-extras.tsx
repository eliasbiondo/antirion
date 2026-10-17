'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, Textarea } from '@/components/ui/form';
import { Dialog } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { GuardrailResource, SafetyEvent } from '@/lib/api/types';
import { formatPercent } from '@/lib/format';
import { notify } from '@/stores/ui';

import { safetyApi } from '../api';
import { ENTITY_NAME, type GuardrailSpec, TYPE_LABEL } from '../rules';
import { CATEGORY_LABEL, useSafety } from '../store';

function listed(lists: { input?: string[]; output?: string[] } | undefined, name: string): string[] {
  const out: string[] = [];
  if (lists?.input?.includes(name)) out.push('prompts');
  if (lists?.output?.includes(name)) out.push('answers');
  return out;
}

/** The policy levels that run a guardrail, as far as the screen has read them. */
export function usePolicyLevel(event: SafetyEvent): string {
  const gateway = useSafety((s) => s.policies.data?.spec.guardrails);
  const teams = useSafety((s) => s.teams.data);
  const models = useSafety((s) => s.models.data);
  const at: string[] = [];
  const g = listed(gateway, event.guardrail);
  if (g.length > 0) at.push(`Gateway (${g.join(' and ')})`);
  const team = teams?.find((t) => t.name === event.team);
  const t = listed(team?.spec.policies?.guardrails, event.guardrail);
  if (team && t.length > 0) at.push(`Team ${team.name} (${t.join(' and ')})`);
  const model = models?.find((m) => m.name === event.model);
  const m = listed(model?.spec.policies?.guardrails, event.guardrail);
  if (model && m.length > 0) at.push(`Model ${model.name} (${m.join(' and ')})`);
  if (at.length > 0) return at.join(' · ');
  return event.organization
    ? `Organization ${event.organization} or a project`
    : 'An organization, team or project';
}

/** What found it: the method and what it looks for. */
function detector(spec: GuardrailSpec | undefined, event: SafetyEvent): string {
  if (!spec) return event.guardrail_type ?? '—';
  const method = TYPE_LABEL[spec.type];
  switch (spec.type) {
    case 'regex': {
      const n = (spec.regex?.allow ?? spec.regex?.deny ?? []).length;
      const check =
        spec.regex?.validate && spec.regex.validate !== 'none'
          ? ` with ${spec.regex.validate === 'luhn' ? 'Luhn' : 'IBAN'} checksum`
          : '';
      return `${spec.regex?.allow?.length ? 'Allow-list' : method}: ${n === 1 ? '1 pattern' : `${n} patterns`}${check}`;
    }
    case 'keywords':
      return `${method}: ${spec.keywords?.words.length ?? 0} ${spec.keywords?.match === 'substring' ? 'anywhere' : 'whole words'}`;
    case 'pii':
    case 'secrets': {
      const found = (event.entities ?? []).map((e) => ENTITY_NAME[e.type] ?? e.type);
      return found.length > 0 ? `${method}: ${found.join(', ')}` : method;
    }
    case 'classifier':
    case 'webhook':
      return `${method}: ${CATEGORY_LABEL[event.category] ?? event.category}`;
    default:
      return method;
  }
}

function reversibility(spec: GuardrailSpec | undefined, event: SafetyEvent): string {
  switch (event.action) {
    case 'block':
      return 'Nothing was sent; the app got an error';
    case 'flag':
      return 'Nothing was changed';
    case 'divert':
      return 'Answered by another Model';
    case 'redact':
      return spec?.pii?.reversible
        ? 'Reversible: the answer got the values back'
        : 'Not reversible: the provider never saw the values';
    default:
      return 'The check could not run';
  }
}

/** Why the guardrail acted: the detector, its score, the policy that runs it and what can be undone. */
export function Rationale({ event }: { event: SafetyEvent }) {
  const spec = useSafety((s) => s.guardrails.data?.find((g) => g.name === event.guardrail)?.spec);
  const level = usePolicyLevel(event);
  const items = [
    { label: 'Detector', value: detector(spec, event) },
    {
      label: 'Score',
      value:
        event.score != null ? event.score.toFixed(2) : `${formatPercent(event.confidence, 0)} confidence`,
    },
    { label: 'Policy level', value: level },
    { label: 'Reversibility', value: reversibility(spec, event) },
  ];
  return (
    <dl
      aria-label="Rationale"
      className="grid grid-cols-1 gap-px overflow-hidden rounded-sm border border-line bg-line sm:grid-cols-2 xl:grid-cols-4"
    >
      {items.map((i) => (
        <div key={i.label} className="flex min-w-0 flex-col gap-0.5 bg-sunken px-3 py-2">
          <dt className="text-caption text-muted">{i.label}</dt>
          <dd className="text-ui break-words text-fg">{i.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** Sends the owning team a notification about the event, with a link back to it. */
export function NotifyDialog({
  event,
  summary,
  open,
  onClose,
}: {
  event: SafetyEvent;
  summary: string;
  open: boolean;
  onClose: () => void;
}) {
  const link = typeof window === 'undefined' ? '' : window.location.href;
  const [subject, setSubject] = useState(`Safety event: ${summary}`.slice(0, 200));
  const [message, setMessage] = useState(
    [
      `${summary}.`,
      `Guardrail ${event.guardrail} ${event.action === 'block' ? 'blocked' : event.action === 'redact' ? 'redacted' : event.action === 'divert' ? 'diverted' : 'flagged'} a request${event.key ? ` from key ${event.key}` : ''}${event.model ? ` to ${event.model}` : ''}.`,
      'Please check whether your app should send this, and reply to the security team if it should.',
    ].join('\n'),
  );
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const team = event.team ?? '';
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Notify team ${team}`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            icon="send"
            busy={busy}
            disabled={!subject.trim() || !message.trim()}
            disabledReason="Write a subject and a message"
            onClick={() => {
              setBusy(true);
              setError(null);
              safetyApi
                .notify({
                  to: { team },
                  subject: subject.trim(),
                  message: message.trim(),
                  ...(link ? { link } : {}),
                })
                .then((r) => {
                  notify(
                    'success',
                    r.recipients === 0 ? 'Nobody to notify' : 'Team notified',
                    r.recipients === 0
                      ? `Team ${team} has no one with email notifications.`
                      : `${r.recipients === 1 ? '1 person' : `${r.recipients} people`} in ${team}.`,
                  );
                  onClose();
                })
                .catch((e: unknown) => setError(e))
                .finally(() => setBusy(false));
            }}
          >
            Send notification
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <p className="text-ui text-muted">
          Everyone in {team} gets it by email and in their notifications, with a link to this event.
        </p>
        <Field label="Subject">
          <Input value={subject} maxLength={200} onChange={(e) => setSubject(e.target.value)} />
        </Field>
        <Field label="Message">
          <Textarea rows={5} maxLength={5000} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
        <InlineError error={error} />
      </div>
    </Dialog>
  );
}

export function guardrailOf(list: GuardrailResource[] | null, name: string): GuardrailResource | null {
  return list?.find((g) => g.name === name) ?? null;
}
