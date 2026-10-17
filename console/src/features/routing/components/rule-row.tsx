'use client';

import Link from 'next/link';
import { type DragEvent, type ReactNode, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { Meter } from '@/components/ui/charts';
import { Icon } from '@/components/ui/icon';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Tooltip } from '@/components/ui/tooltip';
import type { Alert } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, formatPercent, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';

import type { RouteStateEntry } from '../api';
import type { Editor, Impact } from '../insights';
import {
  advancedOf,
  canaryOf,
  describePredicate,
  isRecord,
  mirrorOf,
  nodeTypeLabel,
  type Json,
  type Rule,
  settingsOf,
  sharePercent,
  STRATEGIES,
  strategyOf,
  targetsOf,
} from '../types';

export function strategyLabel(node: Json): string {
  const s = strategyOf(node);
  return STRATEGIES.find((x) => x.value === s)?.label ?? nodeTypeLabel(node);
}

/** A deployment name as a quiet code chip inside a sentence. */
export function Chip({ children }: { children: ReactNode }) {
  return (
    <span className="mono rounded-xs border border-line bg-sunken px-1.5 py-px text-[12px] text-fg">
      {children}
    </span>
  );
}

export function Targets({ node }: { node: Json }) {
  const s = strategyOf(node);
  if (s === 'tier' && isRecord(node) && typeof node.tier === 'string')
    return (
      <>
        the <Chip>{node.tier}</Chip> tier
      </>
    );
  const targets = targetsOf(node);
  if (targets.length === 0) return <span className="text-muted">{nodeTypeLabel(node).toLowerCase()}</span>;
  const sep = s === 'fallback' ? ', then ' : s === 'weighted' || s === 'canary' ? ' · ' : ' / ';
  const mirror = mirrorOf(node);
  return (
    <>
      {targets.map((t, i) => (
        <span key={`${t.deployment}-${i}`}>
          {i > 0 ? <span className="text-muted">{sep}</span> : null}
          <Chip>{t.deployment}</Chip>
          {s === 'weighted' || s === 'canary' ? (
            <span className="mono text-caption text-muted"> {t.weight}</span>
          ) : null}
        </span>
      ))}
      {mirror ? (
        <>
          <span className="text-muted">, copying {sharePercent(mirror.fraction)} to </span>
          <Chip>{mirror.target}</Chip>
        </>
      ) : null}
    </>
  );
}

const CANARY_STATE: Record<string, { label: string; tone: 'warn' | 'error' | 'success' | 'info' }> = {
  ramping: { label: 'Ramping', tone: 'info' },
  held: { label: 'Held', tone: 'warn' },
  rolled_back: { label: 'Rolled back', tone: 'error' },
  complete: { label: 'Ramp complete', tone: 'success' },
};

function Facts({ items }: { items: { label: string; value: ReactNode }[] }) {
  if (items.length === 0) return null;
  return (
    <dl className="grid grid-cols-[minmax(0,auto)_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-ui">
      {items.map((x) => (
        <div key={x.label} className="contents">
          <dt className="text-muted">{x.label}</dt>
          <dd className="min-w-0 text-fg tabular-nums">{x.value}</dd>
        </div>
      ))}
    </dl>
  );
}

function Heading({ children }: { children: ReactNode }) {
  return <h4 className="mb-2 text-caption font-semibold text-muted">{children}</h4>;
}

const PICKER_STATE: Record<string, { label: string; tone: 'success' | 'error' | 'neutral' }> = {
  ok: { label: 'Answering', tone: 'success' },
  failing: { label: 'Failing', tone: 'error' },
  unknown: { label: 'Not asked yet', tone: 'neutral' },
};

/** A pool's external endpoint picker on the answering replica: address, health and what it picked. */
function PickerResult({ picker }: { picker: NonNullable<RouteStateEntry['picker']> }) {
  const state = PICKER_STATE[picker.state] ?? { label: picker.state, tone: 'neutral' as const };
  const pick = picker.last_pick;
  const failure = picker.last_failure;
  return (
    <div>
      <Heading>Endpoint picker · this node</Heading>
      <Facts
        items={[
          { label: 'Address', value: <span className="mono break-all">{picker.url}</span> },
          {
            label: 'State',
            value: (
              <span className="inline-flex flex-wrap items-center gap-2">
                <Badge tone={state.tone}>{state.label}</Badge>
                {picker.last_asked_at ? (
                  <span className="text-caption text-muted">
                    asked {formatRelative(picker.last_asked_at)}
                  </span>
                ) : null}
              </span>
            ),
          },
          {
            label: 'Asked',
            value:
              picker.asks === 0
                ? 'Not yet'
                : `${plural(picker.asks, 'time')}${picker.failures > 0 ? `, ${formatCount(picker.failures)} failed` : ''}`,
          },
          {
            label: 'Last pick',
            value: pick ? (
              pick.deployments.length === 0 ? (
                'No replica (the request got 503)'
              ) : (
                <span className="inline-flex flex-wrap items-center gap-1">
                  {pick.deployments.map((d) => (
                    <Chip key={d}>{d}</Chip>
                  ))}
                  <span className="text-caption text-muted">{formatRelative(pick.at)}</span>
                </span>
              )
            ) : (
              '—'
            ),
          },
          ...(pick?.served ? [{ label: 'Served by', value: <Chip>{pick.served}</Chip> }] : []),
          ...(failure
            ? [
                {
                  label: 'Last failure',
                  value: `${failure.reason} · ${formatRelative(failure.at)}`,
                },
              ]
            : []),
          {
            label: 'If it fails',
            value: picker.on_error === 'builtin' ? 'The gateway picks' : 'Requests are refused (503)',
          },
        ]}
      />
    </div>
  );
}

/** Canary results (RO-29) and shadow results (RO-30) of the rule's node on the answering replica. */
function Results({ rule, result }: { rule: Rule; result: RouteStateEntry | undefined }) {
  const canary = canaryOf(rule.target);
  const mirror = mirrorOf(rule.target);
  if (result?.picker && !canary && !mirror) return <PickerResult picker={result.picker} />;
  if (!canary && !mirror) return null;
  if (!result)
    return (
      <div>
        <Heading>{canary ? 'Canary' : 'Shadow comparison'}</Heading>
        <p className="text-ui text-muted">
          No results on this node yet; they appear after the first matching requests.
        </p>
      </div>
    );
  if (result.canary) {
    const c = result.canary;
    const state = CANARY_STATE[c.state] ?? { label: c.state, tone: 'info' as const };
    const budget = canary?.budget?.max_error_rate ?? null;
    const step = canary ? canary.steps.filter((s) => s.weight <= c.weight).length : null;
    return (
      <div>
        <Heading>Canary</Heading>
        <Facts
          items={[
            { label: 'State', value: <Badge tone={state.tone}>{state.label}</Badge> },
            {
              label: 'Step',
              value: step && canary ? `${step} of ${canary.steps.length}` : '—',
            },
            { label: 'Candidate share', value: `${c.weight}%` },
            {
              label: 'Error rate',
              value: (
                <span className={cn(budget !== null && (c.error_rate ?? 0) > budget && 'text-error')}>
                  {c.error_rate === null ? 'Too few requests yet' : formatPercent(c.error_rate, 2)}
                </span>
              ),
            },
            { label: 'Error budget', value: budget === null ? 'None' : formatPercent(budget, 1) },
            {
              label: 'If over budget',
              value: canary?.budget
                ? c.state === 'ramping'
                  ? `Auto-${canary.budget.action === 'hold' ? 'hold' : 'rollback'} armed`
                  : canary.budget.action === 'hold'
                    ? 'Hold the ramp'
                    : 'Roll back'
                : 'Nothing: no error budget',
            },
          ]}
        />
      </div>
    );
  }
  if (result.mirror) {
    const m = result.mirror;
    const cost = Number(m.cost_delta_usd);
    return (
      <div>
        <Heading>Shadow comparison · last 24 hours</Heading>
        <Facts
          items={[
            { label: 'Compared', value: plural(m.comparisons, 'response') },
            {
              label: 'Answers differ',
              value:
                m.divergence_rate === null ? '—' : `${formatPercent(m.divergence_rate, 1)} of comparisons`,
            },
            {
              label: 'Latency',
              value:
                m.latency_delta_p50_ms === null
                  ? '—'
                  : `${Math.abs(Math.round(m.latency_delta_p50_ms))} ms ${m.latency_delta_p50_ms <= 0 ? 'faster' : 'slower'} (median)`,
            },
            {
              label: 'Cost',
              value: Number.isFinite(cost)
                ? `${formatMoney(Math.abs(cost))} ${cost <= 0 ? 'less' : 'more'} than the answers served`
                : '—',
            },
            { label: 'Copied to', value: mirror ? <Chip>{mirror.target}</Chip> : '—' },
          ]}
        />
      </div>
    );
  }
  return null;
}

export interface RuleRowProps {
  rule: Rule;
  position: number;
  count: number;
  canEdit: boolean;
  canReorder: boolean;
  dragging: boolean;
  expanded: boolean;
  impact: Impact | null;
  /** Requests through the route in 24 hours (the bar's scale); null while unknown. */
  total: number | null;
  editor: Editor | null;
  alerts: Alert[];
  result: RouteStateEntry | undefined;
  configHref: string;
  onExpand: () => void;
  /** Copies a link that opens this rule. */
  onCopyLink?: () => void;
  onEdit: () => void;
  onToggle: (on: boolean) => void;
  onMove: (delta: number) => void;
  onDuplicate: () => void;
  onDelete: () => void;
  onDragStart: () => void;
  onDrop: () => void;
}

/** One rule: its sentence, strategy and status, its 24-hour share, and its details when opened. */
export function RuleRow(p: RuleRowProps) {
  const { rule } = p;
  const [over, setOver] = useState(false);
  const blocking = strategyOf(rule.target) === 'block';
  const canary = canaryOf(rule.target);
  const canaryState = p.result?.canary?.state;
  const share = p.impact && p.total ? p.impact.requests / p.total : null;
  const items: MenuItem[] = [
    {
      label: 'Move up',
      icon: 'arrowUp',
      onSelect: () => p.onMove(-1),
      disabled: !p.canReorder || rule.index === 0,
      disabledReason: p.canReorder ? 'Already first' : 'Clear the filters to reorder',
    },
    {
      label: 'Move down',
      icon: 'arrowDown',
      onSelect: () => p.onMove(1),
      disabled: !p.canReorder || rule.index === p.count - 1,
      disabledReason: p.canReorder ? 'Already last' : 'Clear the filters to reorder',
    },
    { label: 'Duplicate', icon: 'copy', onSelect: p.onDuplicate },
    ...(p.onCopyLink ? [{ label: 'Copy link', icon: 'link' as const, onSelect: p.onCopyLink }] : []),
    { label: 'Delete rule', icon: 'trash', danger: true, onSelect: p.onDelete },
  ];
  const detailId = `rule-${rule.index}-detail`;
  const settings = settingsOf(rule.target);
  const advanced = advancedOf(rule.target);
  const targets = targetsOf(rule.target);
  return (
    <li
      draggable={p.canEdit && p.canReorder}
      onDragStart={(e: DragEvent<HTMLLIElement>) => {
        e.dataTransfer.effectAllowed = 'move';
        p.onDragStart();
      }}
      onDragOver={(e) => {
        if (!p.canReorder) return;
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        p.onDrop();
      }}
      className={cn(
        'border-b border-line bg-raised',
        over && 'shadow-[inset_0_2px_0_var(--ant-accent)]',
        p.dragging && 'opacity-50',
        !rule.enabled && 'bg-canvas',
      )}
    >
      <div className="grid grid-cols-[28px_20px_minmax(0,1fr)_150px_auto] items-center gap-3 px-gutter py-3 max-lg:grid-cols-[28px_20px_minmax(0,1fr)_auto] max-sm:grid-cols-[28px_minmax(0,1fr)] max-sm:px-gutter-sm">
        <span className="mono text-center text-title font-semibold text-muted tabular-nums">
          {p.position}
        </span>
        {p.canEdit ? (
          <Tooltip
            content={
              p.canReorder ? 'Drag, or use the arrow keys, to reorder' : 'Clear the filters to reorder'
            }
          >
            <span className="inline-flex [&>button:disabled]:pointer-events-none">
              <button
                type="button"
                aria-label={`Reorder ${rule.name}: use the arrow keys`}
                disabled={!p.canReorder}
                onKeyDown={(e) => {
                  if (e.key === 'ArrowUp' && rule.index > 0) {
                    e.preventDefault();
                    p.onMove(-1);
                  }
                  if (e.key === 'ArrowDown' && rule.index < p.count - 1) {
                    e.preventDefault();
                    p.onMove(1);
                  }
                }}
                className="flex h-8 w-5 cursor-grab items-center justify-center rounded-xs text-dim hover:text-fg disabled:cursor-not-allowed disabled:opacity-40 max-sm:hidden"
              >
                <Icon name="grip" size={16} />
              </button>
            </span>
          </Tooltip>
        ) : (
          <span className="max-sm:hidden" />
        )}
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className={cn('text-body font-semibold', rule.enabled ? 'text-fg' : 'text-muted')}>
              {rule.name}
            </h3>
            <Badge tone={blocking ? 'error' : 'accent'}>{strategyLabel(rule.target)}</Badge>
            {!rule.enabled ? <Badge>Off</Badge> : null}
            {p.alerts.length > 0 ? (
              <Tooltip content={p.alerts.map((a) => a.summary).join(' · ')}>
                <span>
                  <Badge tone="error" dot>
                    Alerting
                  </Badge>
                </span>
              </Tooltip>
            ) : null}
            {canary?.budget && canaryState === 'ramping' ? (
              <Badge tone="warn">Auto-rollback armed</Badge>
            ) : null}
            {canaryState === 'rolled_back' || canaryState === 'held' ? (
              <Badge tone="error">{CANARY_STATE[canaryState]?.label}</Badge>
            ) : null}
            {rule.tags.map((t) => (
              <Badge key={t}>{t}</Badge>
            ))}
          </div>
          <p className="mt-1 text-ui leading-6 text-muted">
            {rule.when === undefined ? (
              'Every request'
            ) : (
              <>
                When <Chip>{describePredicate(rule.when)}</Chip>
              </>
            )}
            {blocking ? (
              <span className="text-error"> → block the request</span>
            ) : (
              <>
                {' → '}
                <Targets node={rule.target} />
              </>
            )}
          </p>
          {rule.description ? <p className="mt-0.5 text-caption text-dim">{rule.description}</p> : null}
        </div>
        <div className="flex flex-col gap-1 max-lg:hidden">
          {p.impact && p.total !== null ? (
            <>
              <Meter
                value={p.impact.requests}
                max={Math.max(1, p.total)}
                label={`${rule.name}: share of 24 hours`}
              />
              <span className="mono text-caption text-muted tabular-nums">
                {share === null ? '0%' : formatPercent(share, 1)} ·{' '}
                {formatCount(p.impact.requests, { compact: true })} / 24h
              </span>
            </>
          ) : (
            <span className="text-caption text-dim">{p.total === null ? '' : 'No traffic in 24h'}</span>
          )}
        </div>
        <div className="flex items-center gap-1.5 max-sm:col-start-2">
          <Switch
            label={`${rule.enabled ? 'Turn off' : 'Turn on'} ${rule.name}`}
            hideLabel
            checked={rule.enabled}
            disabled={!p.canEdit}
            disabledReason="You don't have permission to change routing."
            onChange={p.onToggle}
          />
          {p.canEdit ? (
            <>
              <Button size="sm" variant="ghost" icon="edit" onClick={p.onEdit}>
                Edit
              </Button>
              <MenuButton items={items} label={`More for ${rule.name}`} iconOnly />
            </>
          ) : null}
          <IconButton
            icon={p.expanded ? 'chevronUp' : 'chevronDown'}
            label={`${p.expanded ? 'Hide' : 'Show'} details of ${rule.name}`}
            aria-expanded={p.expanded}
            aria-controls={detailId}
            onClick={p.onExpand}
          />
        </div>
      </div>
      {p.expanded ? (
        <div
          id={detailId}
          className="grid gap-6 border-t border-line bg-canvas px-gutter py-4 max-sm:px-gutter-sm sm:pl-[calc(var(--spacing-gutter)+72px)] lg:grid-cols-3"
        >
          <div>
            <Heading>Strategy</Heading>
            <p className="mb-3 text-ui text-muted">
              {STRATEGIES.find((s) => s.value === strategyOf(rule.target))?.description ??
                `${nodeTypeLabel(rule.target)}: edited in the configuration.`}
            </p>
            <Facts
              items={
                settings.length > 0
                  ? settings
                  : [{ label: 'Timeouts and retries', value: 'Gateway defaults' }]
              }
            />
          </div>
          <div>
            <Heading>Targets</Heading>
            {targets.length > 0 ? (
              <ul className="flex flex-col gap-1.5 text-ui">
                {targets.map((t, i) => (
                  <li key={`${t.deployment}-${i}`} className="flex items-center gap-2">
                    <Chip>{t.deployment}</Chip>
                    {t.role ? <Badge tone={t.role === 'canary' ? 'warn' : 'neutral'}>{t.role}</Badge> : null}
                    {strategyOf(rule.target) === 'weighted' || canary ? (
                      <span className="mono text-caption text-muted">weight {t.weight}</span>
                    ) : null}
                  </li>
                ))}
                {mirrorOf(rule.target) ? (
                  <li className="flex items-center gap-2">
                    <Chip>{mirrorOf(rule.target)?.target}</Chip>
                    <Badge tone="info">shadow</Badge>
                    <span className="mono text-caption text-muted">
                      {sharePercent(mirrorOf(rule.target)?.fraction ?? 0)} of requests copied
                    </span>
                  </li>
                ) : null}
              </ul>
            ) : (
              <p className="text-ui text-muted">
                <Targets node={rule.target} />
              </p>
            )}
            {advanced.length > 0 ? (
              <div className="mt-4">
                <Heading>Advanced routing</Heading>
                <Facts items={advanced} />
                {strategyOf(rule.target) === 'other' ? (
                  <Link
                    href={p.configHref}
                    className="mt-2 inline-flex items-center gap-1 text-caption text-accent hover:underline"
                  >
                    Change in the configuration <Icon name="arrowRight" size={12} />
                  </Link>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="flex flex-col gap-4">
            <div>
              <Heading>Impact · last 24 hours</Heading>
              <Facts
                items={[
                  {
                    label: 'Matched',
                    value: p.impact
                      ? `${plural(p.impact.requests, 'request')}${share !== null ? ` (${formatPercent(share, 1)} of the route)` : ''}`
                      : p.total === null
                        ? 'Not available'
                        : 'None',
                  },
                  ...(p.impact && p.impact.blocks > 0
                    ? [{ label: 'Blocked', value: formatCount(p.impact.blocks) }]
                    : []),
                  ...(p.impact && p.impact.fallbacks > 0
                    ? [{ label: 'Fell back', value: formatCount(p.impact.fallbacks) }]
                    : []),
                  {
                    label: 'Last changed',
                    value: p.editor
                      ? `${formatRelative(p.editor.time)} by ${p.editor.actor}`
                      : 'Not in the audit log',
                  },
                ]}
              />
            </div>
            <Results rule={rule} result={p.result} />
          </div>
        </div>
      ) : null}
    </li>
  );
}
