'use client';

import { BarList } from '@/components/ui/charts';
import { Panel } from '@/components/ui/layout';
import { Banner } from '@/components/ui/states';
import type { GuardrailResource, SafetyEvent } from '@/lib/api/types';
import { formatPercent, plural } from '@/lib/format';

import type { TopicShare } from '../api';
import { useSafety } from '../store';

/** How an event's request was handled when that was more than a plain block, redaction or flag. */
export function handling(
  event: Pick<SafetyEvent, 'action' | 'guardrail' | 'stage'>,
  guardrails: GuardrailResource[] | null,
): { title: string; body: string } | null {
  const spec = guardrails?.find((g) => g.name === event.guardrail)?.spec as
    { pii?: { reversible?: boolean }; divert_to?: string } | undefined;
  if (event.action === 'redact' && spec?.pii?.reversible) {
    return {
      title: 'Reversible redaction',
      body: 'The provider saw numbered tokens such as [EMAIL_1] instead of the values, and the answer had the original values put back. The values were kept only while the request ran.',
    };
  }
  if (event.action === 'divert') {
    const to = spec?.divert_to;
    return {
      title: to ? `Served by ${to}` : 'Served by the safe Model',
      body: to
        ? `Instead of blocking the request, the gateway sent it to ${to}, the Model this guardrail diverts to, and that Model answered.`
        : 'Instead of blocking the request, the gateway sent it to the Model this guardrail diverts to.',
    };
  }
  return null;
}

/** The note an event's detail shows for a reversible redaction or a diverted request. */
export function HandlingNote({ event }: { event: SafetyEvent }) {
  const guardrails = useSafety((s) => s.guardrails.data);
  const note = handling(event, guardrails);
  if (!note) return null;
  return (
    <Banner tone="info" title={note.title}>
      {note.body}
    </Banner>
  );
}

/** Each topic's share of the topic-restriction findings. */
export function shares(rows: TopicShare[]): { name: string; value: number }[] {
  const total = rows.reduce((a, r) => a + r.events, 0);
  return rows.map((r) => ({ name: r.topic, value: total > 0 ? r.events / total : 0 }));
}

/** What the topic restrictions caught over the last 7 days, by topic; hidden while there is nothing. */
export function TopicsPanel() {
  const topics = useSafety((s) => s.topics.data);
  if (!topics || topics.length === 0) return null;
  const total = topics.reduce((a, r) => a + r.events, 0);
  return (
    <Panel
      title="Off-topic requests by topic"
      subtitle={`Last 7 days · ${plural(total, 'request')} caught by topic restrictions`}
    >
      <BarList
        label="Off-topic requests by topic"
        items={shares(topics).slice(0, 8)}
        format={(v) => formatPercent(v, 0)}
      />
    </Panel>
  );
}
