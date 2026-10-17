'use client';

import { Dot, type Tone } from '@/components/ui/badge';
import { BarList } from '@/components/ui/charts';
import { KpiTile } from '@/components/ui/kpi';
import { TileGrid } from '@/components/ui/layout';
import { ProviderMark } from '@/components/ui/marks';
import { isUnavailable } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import type { AlertRuleResource } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, formatPercent, formatSigned, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';

import { catalogSync, change, leader, worstErrorRate } from '../catalog';
import { useCatalog } from '../store';
import { type ModelView, statusOf, STATUS_LABEL, STATUS_TONE } from '../types';
import { auditEntryLabel, auditActorBy } from '@/lib/audit-words';

const STATUS_ORDER = ['available', 'preview', 'restricted', 'deprecated', 'off'] as const;

const BAR: Record<Tone, string> = {
  success: 'bg-success',
  info: 'bg-info',
  warn: 'bg-warn',
  neutral: 'bg-dim',
  error: 'bg-error',
  accent: 'bg-accent',
};

/** The threshold of the gateway-wide error-rate rule (no scope, or the Model's), if one is on. */
export function errorRateThreshold(rules: AlertRuleResource[] | null, model?: string): number | null {
  const on = (rules ?? []).filter(
    (r) => r.spec.enabled !== false && r.spec.kind === 'metric' && r.spec.metric === 'error_rate',
  );
  const scoped = model ? on.find((r) => r.spec.scope?.model === model) : undefined;
  const global = on.find((r) => !r.spec.scope || Object.keys(r.spec.scope).length === 0);
  const rule = scoped ?? global;
  return typeof rule?.spec.threshold === 'number' ? rule.spec.threshold : null;
}

/** The catalog tile: how many models, how they split by status, and the routes and aliases. */
function CatalogTile({
  views,
  providers,
  unused,
}: {
  views: ModelView[];
  providers: number;
  unused: number;
}) {
  const routes = useCatalog((s) => s.routes.data);
  const audit = useCatalog((s) => s.catalogAudit.data);
  const status = useCatalog((s) => s.catalogStatus.data);
  const sync = status ? catalogSync(status) : null;
  const counts = STATUS_ORDER.map((s) => ({ s, n: views.filter((v) => statusOf(v) === s).length })).filter(
    (c) => c.n > 0,
  );
  const aliases = views.reduce((a, v) => a + v.aliases.length, 0);
  const last = (audit ?? []).find((e) => /^(routes|models)\//.test(e.resource ?? ''));
  return (
    <div className="flex min-w-0 flex-col bg-raised px-4 pt-3.5 pb-3">
      <div className="text-caption font-medium text-muted">Models</div>
      <div className="mono mt-1.5 text-kpi leading-7 font-semibold tracking-[-0.01em] text-fg tabular-nums">
        {views.length}
        <span className="ml-1.5 font-sans text-ui font-normal text-muted">
          from {plural(providers, 'provider')}
        </span>
        {unused > 0 ? (
          <Tooltip content="Providers with no model behind them yet">
            <span className="ml-1.5 font-sans text-caption font-normal text-dim">+{unused} unused</span>
          </Tooltip>
        ) : null}
      </div>
      <div className="mt-2 flex h-1.5 overflow-hidden rounded-full bg-hover" aria-hidden="true">
        {counts.map((c) => (
          <span
            key={c.s}
            className={cn('h-full', BAR[STATUS_TONE[c.s]])}
            ref={(el) => {
              if (el) el.style.width = `${(c.n / Math.max(1, views.length)) * 100}%`;
            }}
          />
        ))}
      </div>
      <ul className="mt-2 flex flex-wrap gap-x-3 gap-y-0.5 text-caption text-muted">
        {counts.map((c) => (
          <li key={c.s} className="flex items-center gap-1.5">
            <Dot tone={STATUS_TONE[c.s]} />
            <span className="mono text-fg tabular-nums">{c.n}</span> {STATUS_LABEL[c.s].toLowerCase()}
          </li>
        ))}
      </ul>
      <div className="mt-auto pt-2 text-caption text-muted">
        {plural(routes?.length ?? 0, 'shared route')} · {plural(aliases, 'alias', 'aliases')}
        {last ? (
          <Tooltip content={`${auditEntryLabel(last)} by ${auditActorBy(last.actor)}`}>
            <span> · changed {formatRelative(last.time)}</span>
          </Tooltip>
        ) : null}
        {sync ? (
          <Tooltip content={sync.detail}>
            <span>
              {' · '}
              <span className="whitespace-nowrap">{sync.text}</span>
              {sync.at ? `, updated ${formatRelative(sync.at)}` : ''}
            </span>
          </Tooltip>
        ) : null}
      </div>
    </div>
  );
}

/** Sums the per-model daily rows into one series per day. */
function dailyTotals(
  usage: Map<string, { days: { start: string; requests: number; cost: number; errors: number }[] }>,
) {
  const byDay = new Map<string, { requests: number; cost: number; errors: number }>();
  for (const u of usage.values())
    for (const d of u.days) {
      const prev = byDay.get(d.start) ?? { requests: 0, cost: 0, errors: 0 };
      byDay.set(d.start, {
        requests: prev.requests + d.requests,
        cost: prev.cost + d.cost,
        errors: prev.errors + d.errors,
      });
    }
  return [...byDay.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([, v]) => v);
}

function deltaOf(current: number, previous: number | null, upIsBad: boolean) {
  const c = change(current, previous);
  if (c === null) return null;
  const text = formatSigned(c, 1, { percent: true });
  const tone: 'good' | 'bad' | 'neutral' =
    Math.abs(c) < 0.005 ? 'neutral' : c > 0 === upIsBad ? 'bad' : 'good';
  return { text, tone, title: 'against the 30 days before' };
}

/**
 * The key figures: the catalog, 30-day requests (with the change on the 30 days before), spend with
 * the leading Model's share, the error rate with the worst Model against the error-rate rule, and
 * requests per provider as bars that filter the list.
 */
export function ModelsHero({
  views,
  providerType,
  provider,
  onProvider,
}: {
  views: ModelView[];
  providerType: (name: string | undefined) => string | undefined;
  provider: string;
  onProvider: (name: string) => void;
}) {
  const usage = useCatalog((s) => s.usage);
  const previous = useCatalog((s) => s.previous.data);
  const rules = useCatalog((s) => s.rules.data);
  const providers = useCatalog((s) => s.providers.data);
  const days = usage.data ? dailyTotals(usage.data) : [];
  const total = days.reduce(
    (a, d) => ({ requests: a.requests + d.requests, cost: a.cost + d.cost, errors: a.errors + d.errors }),
    { requests: 0, cost: 0, errors: 0 },
  );
  const before = previous
    ? [...previous.values()].reduce(
        (a, d) => ({ requests: a.requests + d.requests, cost: a.cost + d.cost, errors: a.errors + d.errors }),
        { requests: 0, cost: 0, errors: 0 },
      )
    : null;
  const usageState = usage.error
    ? isUnavailable(usage.error)
      ? 'unavailable'
      : 'empty'
    : !usage.data
      ? 'loading'
      : total.requests === 0
        ? 'empty'
        : 'ready';
  const rate = total.errors / Math.max(1, total.requests);
  const threshold = errorRateThreshold(rules);
  const lead = usage.data ? leader(usage.data) : null;
  const worst = usage.data ? worstErrorRate(usage.data) : null;
  // The providers behind the listed models; one that serves none is counted apart, so the tile, the
  // filter and the bars agree.
  const providerNames = [...new Set(views.flatMap((v) => v.providers))].sort();
  const unusedProviders = (providers ?? []).filter((p) => !providerNames.includes(p.name)).length;
  const byProvider = providerNames
    .map((p) => ({
      name: p,
      value:
        usageState === 'ready'
          ? // Requests that reached the provider: one refused before routing names no provider.
            views.reduce((a, v) => a + (usage.data?.get(v.name)?.providers[p] ?? 0), 0)
          : views.filter((v) => v.providers.includes(p)).length,
      swatch: <ProviderMark type={providerType(p)} className="text-muted" />,
    }))
    .sort((a, b) => b.value - a.value);

  return (
    <TileGrid
      label="Key figures"
      cols={{ base: 2, md: 3, xl: 5 }}
      template="grid-cols-2 md:grid-cols-3 xl:grid-cols-[repeat(4,minmax(0,1fr))_minmax(0,1.5fr)]"
    >
      <CatalogTile views={views} providers={providerNames.length} unused={unusedProviders} />
      <KpiTile
        label="Requests, last 30 days"
        icon="activity"
        state={usageState}
        emptyHint="After the first request"
        value={formatCount(total.requests, { compact: true })}
        delta={deltaOf(total.requests, before?.requests ?? null, false)}
        context={`${formatCount(total.requests / Math.max(1, days.length), { compact: true })} a day`}
        spark={{ values: days.map((d) => d.requests) }}
      />
      <KpiTile
        label="Spend, last 30 days"
        icon="dollar"
        state={usageState}
        emptyHint="After the first request"
        value={formatMoney(total.cost, { compact: true })}
        delta={deltaOf(total.cost, before?.cost ?? null, true)}
        context={lead ? `${lead.name} leads with ${formatPercent(lead.share, 0)}` : "At each model's prices"}
        spark={{ values: days.map((d) => d.cost), tone: 'info' }}
      />
      <KpiTile
        label="Error rate, last 30 days"
        icon="alert"
        state={usageState}
        emptyHint="After the first request"
        value={formatPercent(rate, 2)}
        valueTone={rate > (threshold ?? 0.01) ? 'warn' : 'neutral'}
        context={
          worst && worst.rate > 0 ? (
            <Tooltip
              content={`${worst.name} fails most, ${formatPercent(worst.rate, 2)} of its requests${threshold !== null ? `; the error-rate rule fires above ${formatPercent(threshold, 1)}` : ''}`}
            >
              <span>
                Worst {worst.name} {formatPercent(worst.rate, 1)}
              </span>
            </Tooltip>
          ) : (
            `${formatCount(total.errors, { compact: true })} failed`
          )
        }
        spark={{
          values: days.map((d) => (d.requests > 0 ? d.errors / d.requests : null)),
          tone: 'error',
        }}
      />
      <div className="col-span-2 flex min-w-0 flex-col bg-raised px-4 pt-3.5 pb-3 md:col-span-1">
        <div className="mb-2 text-caption font-medium text-muted">
          {usageState === 'ready' ? 'Requests by provider, last 30 days' : 'Models by provider'}
        </div>
        <BarList
          label={usageState === 'ready' ? 'Requests by provider' : 'Models by provider'}
          items={byProvider.slice(0, 5)}
          format={(v) => (usageState === 'ready' ? formatCount(v, { compact: true }) : plural(v, 'model'))}
          onSelect={onProvider}
          selected={provider || undefined}
        />
      </div>
    </TileGrid>
  );
}
