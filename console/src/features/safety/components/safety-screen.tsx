'use client';

import { useEffect, useMemo, useState } from 'react';

import { useQueryParam, useScopedHref, useTabParam } from '@/components/layout/url-sync';
import { Badge, Dot, type Tone } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Sparkline } from '@/components/ui/charts';
import { Segmented, Select, Switch } from '@/components/ui/form';
import { useSwitchChanges } from '@/components/ui/confirm';
import { Gate, useCan, usePermission } from '@/components/ui/gate';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { KpiStrip, KpiTile } from '@/components/ui/kpi';
import { DefinitionGrid, FilterBar, PageHeader, Panel, Tabs, TileGrid } from '@/components/ui/layout';
import { DetailHeader, DetailSection, MasterDetail, SelectableList } from '@/components/ui/list-detail';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { UpdatedAt } from '@/components/ui/misc';
import {
  Banner,
  EmptyAction,
  EmptyState,
  ErrorState,
  isUnavailable,
  LoadingRows,
  NoMatch,
} from '@/components/ui/states';
import { CountNote, ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import type { SafetyEvent } from '@/lib/api/types';
import { type CsvColumn, exportCsv } from '@/lib/csv';
import { formatCount, formatLatency, formatPercent, plural } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useQueryWindow } from '@/stores/time-range';
import { notify } from '@/stores/ui';

import { draftOf, ENTITY_NAME, impliedCategory, prepareSimilar, TYPE_LABEL } from '../rules';
import { CATEGORY_LABEL, type SafetyReads, severityTone, useSafety } from '../store';
import { CoveragePanel } from './coverage-panel';
import { NotifyDialog, Rationale } from './event-extras';
import { HandlingNote, TopicsPanel } from './handling';
import { ActivityChart, DataFlowPanel, PosturePanel } from './overview-panels';
import { PiiPanel } from './pii-panel';
import { type EditorTarget, RuleEditor } from './rule-editor';

const ACTIONS = ['block', 'redact', 'flag', 'divert'] as const;
const SEVERITIES = ['critical', 'high', 'medium', 'low'] as const;

/** The events' filters as URL parameters, so a copied link keeps them (`entity` from the PII grid). */
const EVENT_FILTERS = { category: 'all', action: '', severity: '', entity: '' };

const ACTION_LABEL: Record<string, string> = {
  block: 'Blocked',
  redact: 'Redacted',
  flag: 'Flagged',
  divert: 'Diverted',
  error: 'Check failed',
};
const ACTION_FILTER_LABEL: Record<string, string> = {
  block: 'Blocked',
  redact: 'Redacted',
  flag: 'Flagged',
  divert: 'Diverted',
};
const SEVERITY_LABEL: Record<string, string> = {
  critical: 'Critical',
  high: 'High',
  medium: 'Medium',
  low: 'Low',
};
const REVIEW_LABEL: Record<SafetyEvent['review_state'], string> = {
  open: 'To review',
  reviewed: 'Reviewed',
  false_positive: 'False positive',
  escalated: 'Escalated',
};
const ENTITY_LABEL: Record<string, [string, string]> = {
  email: ['email', 'emails'],
  phone: ['phone number', 'phone numbers'],
  credit_card: ['card number', 'card numbers'],
  ssn: ['social security number', 'social security numbers'],
  iban: ['IBAN', 'IBANs'],
  ip_address: ['IP address', 'IP addresses'],
  person: ['name', 'names'],
  address: ['street address', 'street addresses'],
  api_key: ['API key', 'API keys'],
};

function actionTone(action: string): Tone {
  return action === 'block'
    ? 'error'
    : action === 'redact'
      ? 'warn'
      : action === 'error'
        ? 'neutral'
        : 'info';
}

function severityDot(severity: string): Tone {
  const tone = severityTone(severity);
  return tone === 'neutral' ? 'neutral' : tone;
}

/** "2 emails and 1 phone number". */
function entitiesText(event: SafetyEvent): string | null {
  const parts = (event.entities ?? []).map((e) => {
    const [one, many] = ENTITY_LABEL[e.type] ?? [e.type.replace(/_/g, ' '), `${e.type.replace(/_/g, ' ')}s`];
    return plural(e.count, one, many);
  });
  if (parts.length === 0) return null;
  if (parts.length === 1) return parts[0] ?? null;
  return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
}

/** Counts per bucket across the window, for the KPI sparklines. */
function buckets(events: SafetyEvent[], from: string, to: string, keep: (e: SafetyEvent) => number) {
  const start = Date.parse(from);
  const span = Math.max(1, Date.parse(to) - start);
  const out = new Array<number>(16).fill(0);
  for (const e of events) {
    const i = Math.min(15, Math.max(0, Math.floor(((Date.parse(e.time) - start) / span) * 16)));
    out[i] = (out[i] ?? 0) + keep(e);
  }
  return out;
}

function EventRow({ event }: { event: SafetyEvent }) {
  const found = entitiesText(event);
  return (
    <div className="flex items-start gap-3">
      <div className="min-w-0 flex-1">
        <p className="truncate text-body text-fg">
          <span className="font-semibold">{event.guardrail}</span>
          {found ? <span className="text-muted"> found {found}</span> : null}
        </p>
        <p className="mt-0.5 truncate text-caption text-muted">
          {formatRelative(event.time)}
          {event.end_user || event.key ? ` · ${event.end_user ?? event.key}` : ''}
          {event.model ? <span className="mono"> · {event.model}</span> : null}
        </p>
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          <Badge>{CATEGORY_LABEL[event.category] ?? event.category}</Badge>
          {event.review_state !== 'open' ? (
            <Badge tone="success">{REVIEW_LABEL[event.review_state]}</Badge>
          ) : null}
        </div>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <Badge tone={actionTone(event.action)} dot>
          {ACTION_LABEL[event.action] ?? event.action}
        </Badge>
        <span className="text-caption text-muted tabular-nums">
          {formatPercent(event.confidence, 0)} confidence
        </span>
      </div>
    </div>
  );
}

function EventDetail({
  event,
  related,
  onEdit,
}: {
  event: SafetyEvent;
  related: SafetyEvent[];
  onEdit: (target: EditorTarget) => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const review = useSafety((s) => s.review);
  const guardrail = useSafety((s) => s.guardrails.data?.find((g) => g.name === event.guardrail) ?? null);
  const attached = useSafety((s) => s.policies.data?.spec.guardrails);
  const canReview = useCan('reviewSafetyEvent');
  const canTune = useCan('replaceGuardrail');
  const canNotify = useCan('sendNotification');
  const [notifying, setNotifying] = useState(false);
  const canContent = usePermission('logs:read_content');
  const logHref = useScopedHref('/request-log/', { request: event.log_id });
  const act = (state: 'reviewed' | 'false_positive' | 'escalated', message: string) =>
    void review(event.id, state).then(() => notify('success', message, event.guardrail));
  const found = entitiesText(event);
  const summary = found ? `${event.guardrail} found ${found}` : event.guardrail;
  const more: MenuItem[] = [];
  if (guardrail && canTune) {
    const draft = draftOf(guardrail.name, guardrail.spec, {
      input: (attached?.input ?? []).includes(guardrail.name),
      output: (attached?.output ?? []).includes(guardrail.name),
    });
    more.push({
      label: 'Tune rule',
      icon: 'edit',
      onSelect: () => onEdit({ kind: 'edit', name: guardrail.name }),
    });
    const allow = prepareSimilar('allow', event, draft);
    const block = prepareSimilar('block', event, draft);
    if (allow)
      more.push({
        label: 'Allow similar',
        icon: 'check',
        onSelect: () => onEdit({ kind: 'edit', name: guardrail.name, prepared: allow }),
      });
    if (block)
      more.push({
        label: 'Block similar',
        icon: 'lock',
        onSelect: () => onEdit({ kind: 'edit', name: guardrail.name, prepared: block }),
      });
  }
  if (canNotify)
    more.push({
      label: event.team ? `Notify ${event.team}` : 'Notify the team',
      icon: 'bell',
      onSelect: () => setNotifying(true),
      disabled: !event.team,
      disabledReason: 'This event has no team',
    });
  if (canReview)
    more.push({ label: 'Escalate', icon: 'alert', onSelect: () => act('escalated', 'Event escalated') });
  more.push({
    label: 'Copy link',
    icon: 'link',
    onSelect: () => void navigator.clipboard.writeText(window.location.href),
  });
  return (
    <div>
      <DetailHeader
        eyebrow={
          <>
            <Badge tone={severityTone(event.severity)} dot>
              {SEVERITY_LABEL[event.severity] ?? event.severity}
            </Badge>
            <Badge tone={actionTone(event.action)}>{ACTION_LABEL[event.action] ?? event.action}</Badge>
            <span className="font-sans">{CATEGORY_LABEL[event.category] ?? event.category}</span>
            <span className="font-sans">· {REVIEW_LABEL[event.review_state]}</span>
          </>
        }
        title={found ? `${event.guardrail} found ${found}` : event.guardrail}
        subtitle={<span className="font-sans">{formatDateTime(event.time, timeZone)}</span>}
        actions={
          <>
            <Gate operation="reviewSafetyEvent" mode="hide">
              <Button
                variant="primary"
                icon="check"
                onClick={() => act('reviewed', 'Marked as reviewed')}
                disabled={event.review_state === 'reviewed'}
                disabledReason="Already reviewed"
              >
                Mark reviewed
              </Button>
            </Gate>
            <Gate operation="reviewSafetyEvent" mode="hide">
              <Button
                onClick={() => act('false_positive', 'Marked as a false positive')}
                disabled={event.review_state === 'false_positive'}
                disabledReason="Already marked as a false positive"
              >
                False positive
              </Button>
            </Gate>
            <span className="ml-auto">
              <MenuButton items={more} />
            </span>
          </>
        }
      />
      {event.action === 'block' ? (
        <Banner tone="error" title="Request blocked">
          Nothing was sent to the provider. Reviewing the event doesn&apos;t change what happened.
        </Banner>
      ) : null}
      <HandlingNote event={event} />
      <DetailSection title="Why">
        <Rationale event={event} />
      </DetailSection>
      <DetailSection title="Details">
        <DefinitionGrid
          columns={3}
          items={[
            { label: 'User', value: event.end_user ?? '—' },
            { label: 'Team', value: event.team ?? '—' },
            { label: 'Key', value: event.key ?? '—' },
            { label: 'Model', value: event.model ?? '—' },
            { label: 'Provider', value: event.provider ?? '—' },
            {
              label: 'Guardrail',
              value: `${event.guardrail}${event.guardrail_type ? ` (${event.guardrail_type})` : ''}`,
            },
            { label: 'Confidence', value: formatPercent(event.confidence, 0) },
            { label: 'Check took', value: formatLatency(event.latency_ms) },
            { label: 'Grouping key', value: event.fingerprint },
          ]}
        />
      </DetailSection>
      <DetailSection
        title="Content"
        aside={
          <a href={logHref} className="text-caption text-accent hover:underline">
            View request
          </a>
        }
      >
        {canContent ? (
          <p className="text-ui text-muted">
            {event.spans && event.spans.length > 0
              ? `${plural(event.spans.length, 'match', 'matches')}: ${event.spans.map((s) => s.entity ?? '').join(', ')}. `
              : ''}
            See the prompt with the matches highlighted on the request, when the gateway keeps prompts.
          </p>
        ) : (
          <p className="text-ui text-muted">Your role can&apos;t see prompt text or the matched values.</p>
        )}
      </DetailSection>
      <DetailSection
        title="Similar events"
        aside={<span className="text-caption text-muted">{related.length}</span>}
      >
        {related.length === 0 ? <p className="text-ui text-muted">None</p> : null}
        <ul className="flex flex-col gap-1.5">
          {related.slice(0, 8).map((r) => (
            <li key={r.id} className="flex items-center gap-2 text-caption text-muted">
              <Dot tone={severityDot(r.severity)} />
              <span className="mono">{r.id}</span>· {formatRelative(r.time)} ·{' '}
              {ACTION_LABEL[r.action] ?? r.action}
            </li>
          ))}
        </ul>
      </DetailSection>
      {notifying ? (
        <NotifyDialog event={event} summary={summary} open onClose={() => setNotifying(false)} />
      ) : null}
    </div>
  );
}

function RulesPanel({ onEdit }: { onEdit: (target: EditorTarget) => void }) {
  const guardrails = useSafety((s) => s.guardrails);
  const runtime = useSafety((s) => s.runtime);
  const setEnabled = useSafety((s) => s.setEnabled);
  const canEdit = useCan('patchGuardrail');
  const canCreate = useCan('createGuardrail');
  const list = guardrails.data ?? [];
  const figures = !runtime.error;
  const on = list.filter((g) => g.spec.enabled !== false).length;
  const attached = useSafety((s) => s.policies.data?.spec.guardrails);
  const switches = useSwitchChanges();
  /** A rule the gateway level attaches checks every request once it is on, so turning it on asks. */
  const everyRequest = (g: (typeof list)[number]) => {
    const stages = [
      (attached?.input ?? []).includes(g.name) ? 'request' : null,
      (attached?.output ?? []).includes(g.name) ? 'answer' : null,
    ].filter((x): x is string => x !== null);
    if (stages.length === 0) return undefined;
    const what = stages.join(' and every ');
    const action = g.spec.action ?? 'block';
    return {
      title: `Turn ${g.name} on for all traffic?`,
      confirmLabel: 'Turn on',
      effect: `Every ${what} will be checked by ${g.name}; ${
        action === 'block'
          ? 'what it matches is refused'
          : action === 'redact'
            ? 'what it matches is redacted'
            : action === 'divert'
              ? 'what it matches goes to another Model'
              : 'what it matches is flagged for review'
      }${g.spec.on_error === 'block' ? ', and requests are refused while the check fails' : ''}.`,
    };
  };
  return (
    <>
      {switches.dialog}
      <Panel
        title="Rules"
        subtitle={
          guardrails.data && list.length > 0 ? `${on} active · ${list.length - on} turned off` : undefined
        }
        bodyClassName="px-0 pb-0"
      >
        {guardrails.status === 'loading' && !guardrails.data ? <LoadingRows rows={3} /> : null}
        {guardrails.error ? <ErrorState error={guardrails.error} subject="Guardrails" /> : null}
        {guardrails.data && list.length === 0 ? (
          <EmptyState
            title="No guardrails yet"
            description="Guardrails check prompts and answers for personal data, secrets and injection."
            action={
              canCreate ? (
                <EmptyAction onClick={() => onEdit({ kind: 'new' })}>Create rule</EmptyAction>
              ) : undefined
            }
          />
        ) : null}
        {list.length > 0 ? (
          <Table label="Guardrails">
            <thead>
              <tr>
                <Th className="w-14">On</Th>
                <Th>Rule</Th>
                <Th>Method</Th>
                <Th>Category</Th>
                <Th>Severity</Th>
                <Th>Action</Th>
                {figures ? (
                  <>
                    <Th numeric>Matches, last 24 h</Th>
                    <Th>Trend</Th>
                    <Th numeric>False positives</Th>
                  </>
                ) : null}
              </tr>
            </thead>
            <tbody>
              {list.map((g) => {
                const rt = runtime.data?.find((r) => r.name === g.name);
                const category = impliedCategory(g.spec);
                const mapped = Object.values(g.spec.categories ?? {});
                const severity = g.spec.severity ?? rt?.severity ?? null;
                return (
                  <Tr key={g.name}>
                    <Td>
                      <Switch
                        label={`Turn ${g.name} ${g.spec.enabled !== false ? 'off' : 'on'}`}
                        hideLabel
                        checked={g.spec.enabled !== false}
                        disabled={!canEdit}
                        disabledReason="Your role can't change guardrails"
                        onChange={(next) =>
                          switches.change({
                            name: g.name,
                            apply: () => setEnabled(g.name, next),
                            undo: () => setEnabled(g.name, !next),
                            done: next ? `${g.name} turned on` : `${g.name} turned off`,
                            undone: next ? `${g.name} turned off` : `${g.name} turned on`,
                            ask: next ? everyRequest(g) : undefined,
                          })
                        }
                      />
                    </Td>
                    <Td>
                      <button
                        type="button"
                        onClick={() => onEdit({ kind: 'edit', name: g.name })}
                        className="mono text-left text-fg hover:text-accent hover:underline"
                      >
                        {g.name}
                      </button>
                      {g.spec.description ? (
                        <span className="block max-w-[320px] truncate text-caption text-muted">
                          {g.spec.description}
                        </span>
                      ) : null}
                    </Td>
                    <Td muted>{TYPE_LABEL[g.spec.type]}</Td>
                    <Td muted>
                      {category
                        ? (CATEGORY_LABEL[category] ?? category)
                        : mapped.length > 0
                          ? [...new Set(mapped)].map((c) => CATEGORY_LABEL[c] ?? c).join(', ')
                          : '—'}
                    </Td>
                    <Td>
                      {severity ? (
                        <Badge tone={severityTone(severity)} dot>
                          {SEVERITY_LABEL[severity] ?? severity}
                        </Badge>
                      ) : (
                        <span className="text-muted">—</span>
                      )}
                    </Td>
                    <Td>
                      {g.spec.action ? (
                        <Badge tone={actionTone(g.spec.action)}>
                          {ACTION_FILTER_LABEL[g.spec.action] ?? g.spec.action}
                        </Badge>
                      ) : (
                        <span className="text-muted">Changes requests</span>
                      )}
                    </Td>
                    {figures ? (
                      <>
                        <Td numeric>{rt ? formatCount(rt.hits_24h) : '—'}</Td>
                        <Td className="w-32">
                          {rt?.hits_by_hour ? (
                            <Sparkline
                              values={rt.hits_by_hour}
                              tone="warn"
                              label={`${g.name} matches per hour`}
                            />
                          ) : null}
                        </Td>
                        <Td numeric>
                          {rt?.false_positive_rate != null ? formatPercent(rt.false_positive_rate, 1) : '—'}
                        </Td>
                      </>
                    ) : null}
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : null}
      </Panel>
    </>
  );
}

function SafetySummary({ list, from, to }: { list: SafetyEvent[]; from: string; to: string }) {
  const redacted = (e: SafetyEvent) =>
    e.action === 'redact' ? (e.entities ?? []).reduce((x, en) => x + en.count, 0) : 0;
  const labelled = list.filter((e) => e.review_state === 'reviewed' || e.review_state === 'false_positive');
  const precision = labelled.length
    ? 1 - labelled.filter((e) => e.review_state === 'false_positive').length / labelled.length
    : null;
  const criticalOpen = list.filter((e) => e.severity === 'critical' && e.review_state === 'open').length;
  const open = list.filter((e) => e.review_state === 'open').length;
  return (
    <KpiStrip label="Safety summary" columns={5}>
      <KpiTile
        label="Values redacted"
        icon="shield"
        value={formatCount(list.reduce((a, e) => a + redacted(e), 0))}
        context={`in ${plural(list.filter((e) => e.action === 'redact').length, 'request')}`}
        spark={{ values: buckets(list, from, to, redacted), tone: 'accent' }}
      />
      <KpiTile
        label="Critical to review"
        icon="alert"
        value={criticalOpen}
        valueTone={criticalOpen > 0 ? 'error' : 'neutral'}
        context={`${list.filter((e) => e.severity === 'critical').length} critical in all`}
        spark={{
          values: buckets(list, from, to, (e) => (e.severity === 'critical' ? 1 : 0)),
          tone: 'error',
        }}
      />
      <KpiTile
        label="Prompt injections blocked"
        icon="lock"
        value={list.filter((e) => e.category === 'prompt_injection' && e.action === 'block').length}
        context={`${list.filter((e) => e.action === 'block').length} requests blocked in all`}
        spark={{
          values: buckets(list, from, to, (e) => (e.category === 'prompt_injection' ? 1 : 0)),
          tone: 'warn',
        }}
      />
      <KpiTile
        label="To review"
        icon="list"
        value={open}
        context="Reviews tune the rules"
        spark={{ values: buckets(list, from, to, (e) => (e.review_state === 'open' ? 1 : 0)), tone: 'info' }}
      />
      <KpiTile
        label="Precision"
        icon="check"
        state={precision === null ? 'empty' : 'ready'}
        emptyHint="After the first review"
        value={precision === null ? undefined : formatPercent(precision, 1).replace('%', '')}
        unit="%"
        context={`from ${plural(labelled.length, 'reviewed event')}`}
      />
    </KpiStrip>
  );
}

const CSV_COLUMNS: CsvColumn<SafetyEvent>[] = [
  { header: 'Time', value: (e) => e.time },
  { header: 'Event', value: (e) => e.id },
  { header: 'Guardrail', value: (e) => e.guardrail },
  { header: 'Category', value: (e) => CATEGORY_LABEL[e.category] ?? e.category },
  { header: 'Action', value: (e) => ACTION_LABEL[e.action] ?? e.action },
  { header: 'Severity', value: (e) => SEVERITY_LABEL[e.severity] ?? e.severity },
  { header: 'Confidence', value: (e) => e.confidence },
  { header: 'Found', value: (e) => (e.entities ?? []).map((x) => `${x.type}:${x.count}`).join(' ') },
  { header: 'Review', value: (e) => REVIEW_LABEL[e.review_state] },
  { header: 'Team', value: (e) => e.team ?? '' },
  { header: 'Key', value: (e) => e.key ?? '' },
  { header: 'User', value: (e) => e.end_user ?? '' },
  { header: 'Model', value: (e) => e.model ?? '' },
  { header: 'Provider', value: (e) => e.provider ?? '' },
  { header: 'Request', value: (e) => e.log_id },
];

type View = 'events' | 'rules' | 'pii' | 'coverage';

/** Safety & PII: what the guardrails redacted, blocked or flagged, each event's detail, and the rules. */
export function SafetyScreen() {
  const window = useQueryWindow();
  const events = useSafety((s) => s.events);
  const load = useSafety((s) => s.load);
  const [view, setView] = useTabParam();
  const [selected, setSelected] = useQueryParam('event');
  const url = useUrlFilters(EVENT_FILTERS);
  const { category, action, severity } = url.filters;
  const entity = url.filters.entity || null;
  const [editing, setEditing] = useState<EditorTarget | null>(null);
  const reads: SafetyReads = {
    policies: useCan('getPoliciesSettings'),
    summary: useCan('getSafetySummary'),
    usage: useCan('getUsageAnalytics'),
    config: useCan('exportConfig'),
    users: useCan('listUsers'),
    keys: useCan('listKeys'),
    teams: useCan('listTeams'),
    models: useCan('listModels'),
  };
  const readsKey = JSON.stringify(reads);
  const canCreate = useCan('createGuardrail');
  const topics = useSafety((s) => (s.topics.data?.length ?? 0) > 0);
  const tab: View = view === 'rules' || view === 'pii' || view === 'coverage' ? view : 'events';
  useEffect(() => {
    void load(window.from, window.to, {
      interval: window.interval,
      reads: JSON.parse(readsKey) as SafetyReads,
    });
  }, [window, load, readsKey]);
  usePolling(
    () =>
      void load(window.from, window.to, {
        quiet: true,
        interval: window.interval,
        reads: JSON.parse(readsKey) as SafetyReads,
      }),
  );

  const list = useMemo(() => events.data ?? [], [events.data]);
  const visible = list.filter(
    (e) =>
      (category === 'all' || e.category === category) &&
      (!action || e.action === action) &&
      (!severity || e.severity === severity) &&
      (!entity || (e.entities ?? []).some((x) => x.type === entity)),
  );
  const current = list.find((e) => e.id === selected) ?? visible[0];
  const categories = [...new Set(list.map((e) => e.category))];
  const filtered = category !== 'all' || action !== '' || severity !== '' || Boolean(entity);
  const clear = () => url.clear();
  usePageShortcut('e', () => {
    if (visible.length > 0) exportCsv('safety-events', CSV_COLUMNS, visible);
  });
  const unavailable = events.error !== null && isUnavailable(events.error);

  return (
    <>
      <PageHeader
        title="Safety & PII"
        description="What the guardrails redacted, blocked or flagged"
        actions={
          <>
            <UpdatedAt at={events.at} />
            {canCreate ? (
              <Button variant="primary" icon="plus" onClick={() => setEditing({ kind: 'new' })}>
                New rule
              </Button>
            ) : null}
          </>
        }
      />
      {list.length > 0 ? <SafetySummary list={list} from={window.from} to={window.to} /> : null}
      {tab === 'events' && (reads.config || reads.usage) ? (
        <TileGrid
          label="Posture and data flow"
          className={reads.config && reads.usage ? 'lg:grid-cols-2' : undefined}
        >
          <PosturePanel />
          <DataFlowPanel />
        </TileGrid>
      ) : null}
      {tab === 'events' && (reads.summary || topics) ? (
        <TileGrid
          label="Events over time"
          className={topics && reads.summary ? 'lg:grid-cols-[2fr_1fr]' : undefined}
        >
          <ActivityChart from={window.from} to={window.to} interval={window.interval} />
          <TopicsPanel />
        </TileGrid>
      ) : null}
      <Tabs<View>
        label="Safety views"
        value={tab}
        onChange={(v) => setView(v === 'events' ? null : v)}
        items={[
          { value: 'events', label: 'Events', count: events.data ? list.length : undefined, icon: 'shield' },
          { value: 'rules', label: 'Rules', icon: 'settings' },
          { value: 'pii', label: 'Personal data', icon: 'user' },
          { value: 'coverage', label: 'Coverage', icon: 'checkCircle' },
        ]}
      />
      <RuleEditor target={editing} onClose={() => setEditing(null)} />
      {tab === 'rules' ? (
        <RulesPanel onEdit={setEditing} />
      ) : tab === 'pii' ? (
        <PiiPanel
          onEdit={setEditing}
          onFilter={(e) => url.setWith({ entity: e }, { tab: null, view: null })}
        />
      ) : tab === 'coverage' ? (
        <CoveragePanel onEdit={setEditing} />
      ) : (
        <>
          {list.length > 0 ? (
            <FilterBar>
              <Segmented<string>
                label="Category"
                value={category}
                onChange={(v) => url.set({ category: v })}
                className="max-w-full overflow-x-auto"
                options={[
                  { value: 'all', label: 'All', count: list.length },
                  ...categories.map((c) => ({
                    value: c,
                    label: CATEGORY_LABEL[c] ?? c,
                    count: list.filter((e) => e.category === c).length,
                  })),
                ]}
              />
              <div className="w-40 max-sm:min-w-[40%] max-sm:flex-1">
                <Select
                  aria-label="Severity"
                  value={severity}
                  onChange={(v) => url.set({ severity: v })}
                  emptyOption="Any severity"
                  options={SEVERITIES.map((s) => ({
                    value: s,
                    label: `${SEVERITY_LABEL[s]} (${list.filter((e) => e.severity === s).length})`,
                  }))}
                />
              </div>
              <div className="w-40 max-sm:min-w-[40%] max-sm:flex-1">
                <Select
                  aria-label="Action"
                  value={action}
                  onChange={(v) => url.set({ action: v })}
                  emptyOption="Any action"
                  options={ACTIONS.map((a) => ({
                    value: a,
                    label: `${ACTION_FILTER_LABEL[a]} (${list.filter((e) => e.action === a).length})`,
                  }))}
                />
              </div>
              {entity ? (
                <Badge tone="info">
                  {ENTITY_NAME[entity] ?? entity}
                  <button
                    type="button"
                    aria-label={`Stop filtering to ${ENTITY_NAME[entity] ?? entity}`}
                    onClick={() => url.set({ entity: '' })}
                    className="ml-1 text-muted hover:text-fg"
                  >
                    ×
                  </button>
                </Badge>
              ) : null}
              {filtered ? (
                <Button size="sm" variant="ghost" onClick={clear}>
                  Clear filters
                </Button>
              ) : null}
              <span className="ml-auto flex items-center gap-2">
                <CountNote shown={visible.length} total={list.length} noun="events" />
                <ExportCsvButton filename="safety-events" columns={CSV_COLUMNS} rows={visible} />
              </span>
            </FilterBar>
          ) : null}
          {events.status === 'loading' && !events.data ? <LoadingRows /> : null}
          {events.error ? (
            <ErrorState
              error={events.error}
              subject="Safety events"
              onRetry={unavailable ? undefined : () => void load(window.from, window.to)}
            />
          ) : null}
          {events.data?.length === 0 ? (
            <EmptyState
              title="Nothing to review"
              description="No guardrail acted on a request in this time range."
              action={<EmptyAction onClick={() => setView('rules')}>View rules</EmptyAction>}
            />
          ) : null}
          {list.length > 0 ? (
            <MasterDetail
              sticky
              narrow={{
                open: Boolean(selected && current),
                onClose: () => setSelected(null),
                title: current?.guardrail ?? '',
              }}
              list={
                visible.length === 0 && filtered ? (
                  <NoMatch onClear={clear} />
                ) : (
                  <SelectableList
                    label="Safety events"
                    items={visible}
                    keyOf={(e) => e.id}
                    selectedKey={current?.id ?? null}
                    onSelect={setSelected}
                    toneOf={(e) => severityDot(e.severity)}
                    renderItem={(e) => <EventRow event={e} />}
                  />
                )
              }
              detail={
                current ? (
                  <EventDetail
                    key={current.id}
                    onEdit={setEditing}
                    event={current}
                    related={list.filter((e) => e.fingerprint === current.fingerprint && e.id !== current.id)}
                  />
                ) : (
                  <EmptyState title="Select an event to see its details" />
                )
              }
            />
          ) : null}
          {list.length > 0 ? <KeyboardHints keys={['move', 'open', 'close', 'export']} /> : null}
        </>
      )}
    </>
  );
}
