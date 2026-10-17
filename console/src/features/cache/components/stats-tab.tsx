'use client';

import { useMemo } from 'react';

import { Badge } from '@/components/ui/badge';
import { BarRow } from '@/components/ui/charts';
import { Panel } from '@/components/ui/layout';
import { EmptyState, ErrorState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatCount, formatMoney, formatPercent, formatTokens, plural } from '@/lib/format';

import type { EffectivePrice } from '../api';
import { describeCache, prefixRows } from '../policy';
import { useCache } from '../store';

const FP_WARN = 0.05;

/** Similar-prompt matching per configuration level: how often it hits, how close, and how often wrongly. */
function SemanticPanel() {
  const status = useCache((s) => s.status);
  const groups = status.data?.semantic ?? [];
  return (
    <Panel
      title="Similar-prompt matching"
      subtitle="Last 30 days, per level that turns it on"
      bodyClassName="px-0 pb-0"
    >
      {status.error ? <ErrorState error={status.error} subject="Matching statistics" size="sm" /> : null}
      {status.data && groups.length === 0 ? (
        <EmptyState
          size="sm"
          title="No model matches similar prompts"
          description="Turn it on in a model’s cache settings."
        />
      ) : null}
      {groups.length > 0 ? (
        <div className="overflow-x-auto">
          <Table label="Similar-prompt matching">
            <thead>
              <tr>
                <Th>Where</Th>
                <Th>Embedding model</Th>
                <Th numeric>Threshold</Th>
                <Th numeric>Hits</Th>
                <Th numeric>Mean similarity</Th>
                <Th numeric title="Hits people reported as answering a different question">
                  Wrong matches
                </Th>
              </tr>
            </thead>
            <tbody>
              {groups.map((g) => {
                const [level, ...rest] = g.scope.split('/');
                const high = g.false_positive_rate !== null && g.false_positive_rate > FP_WARN;
                return (
                  <Tr key={g.scope}>
                    <Td>
                      {g.scope === 'gateway' ? (
                        'Gateway default'
                      ) : (
                        <>
                          <span className="text-muted">{level} </span>
                          <span className="mono">{rest.join('/')}</span>
                        </>
                      )}
                    </Td>
                    <Td mono muted>
                      {g.embedding_model ?? '—'}
                    </Td>
                    <Td numeric>{g.threshold.toFixed(2)}</Td>
                    <Td numeric>{formatCount(g.hits)}</Td>
                    <Td numeric>
                      {g.mean_similarity_of_hits !== null ? g.mean_similarity_of_hits.toFixed(3) : '—'}
                    </Td>
                    <Td numeric className={high ? 'text-warn' : undefined}>
                      {g.false_positive_rate !== null
                        ? `${formatPercent(g.false_positive_rate, 1)} (${formatCount(g.false_positive_reports)})`
                        : formatCount(g.false_positive_reports)}
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
          {groups.some((g) => g.false_positive_rate !== null && g.false_positive_rate > FP_WARN) ? (
            <p className="px-5 py-2.5 text-caption text-warn">
              More than 5% of hits were reported wrong somewhere: raise that threshold so only closer prompts
              match.
            </p>
          ) : null}
        </div>
      ) : null}
    </Panel>
  );
}

/**
 * The statistics take a few seconds (30 days grouped by team and by prefix): the tab keeps its
 * sections, each with placeholder rows, while they load.
 */
function StatsSkeleton() {
  const section = (title: string, subtitle: string, className?: string) => (
    <Panel title={title} subtitle={subtitle} bodyClassName="px-0 pb-0" className={className}>
      <LoadingRows rows={3} label={`Loading ${title.toLowerCase()}`} />
    </Panel>
  );
  return (
    <div className="grid grid-cols-1 gap-px border-b border-line bg-line xl:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]">
      {section('Top saving teams', 'Last 30 days')}
      <SemanticPanel />
      {section(
        'Provider prompt caching by prefix',
        'Last 30 days; saved is cached reads at the input price less the cached price',
        'xl:col-span-2',
      )}
      {section(
        'Overrides',
        'Organizations, teams, projects, keys and routes with their own cache settings',
        'xl:col-span-2',
      )}
    </div>
  );
}

/** Statistics: top saving teams, similar-prompt matching, provider prompt caching by prefix, overrides. */
export function StatsTab() {
  const stats = useCache((s) => s.stats);
  const prices = useCache((s) => s.prices.data);
  const prefixes = useMemo(
    () => (stats.data ? prefixRows(stats.data.prefixes, prices ?? new Map<string, EffectivePrice>()) : []),
    [stats.data, prices],
  );
  const teams = stats.data?.teams ?? [];
  const overrides = stats.data?.overrides ?? [];
  const gateway = stats.data?.gateway;
  const shownPrefixes = prefixes.slice(0, 12);
  const topTeams = teams.filter((t) => t.saved > 0).slice(0, 8);

  if (!stats.data && !stats.error) return <StatsSkeleton />;
  if (stats.error)
    return isUnavailable(stats.error) ? (
      <UnavailableState subject="Cache statistics" />
    ) : (
      <ErrorState error={stats.error} subject="Cache statistics" />
    );

  return (
    <div className="grid grid-cols-1 gap-px border-b border-line bg-line xl:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]">
      <Panel title="Top saving teams" subtitle="Last 30 days">
        {teams.length === 0 || teams.every((t) => t.saved === 0) ? (
          <EmptyState size="sm" title="No savings yet" />
        ) : (
          <ul aria-label="Saved by team, last 30 days" className="flex flex-col gap-1">
            {topTeams.map((t) => (
              <BarRow
                key={t.team}
                name={<span className="mono truncate">{t.team}</span>}
                value={t.saved}
                max={topTeams[0]?.saved ?? 1}
                tone="success"
                display={
                  <>
                    {formatMoney(t.saved)}
                    <span className="ml-2 font-sans text-muted">
                      {formatPercent(t.requests > 0 ? t.hits / t.requests : 0, 0)} hits
                    </span>
                  </>
                }
                label={`${t.team}: saved ${formatMoney(t.saved)}, ${formatCount(t.hits)} of ${formatCount(t.requests)} requests from the cache`}
              />
            ))}
          </ul>
        )}
      </Panel>
      <SemanticPanel />
      <Panel
        title="Provider prompt caching by prefix"
        subtitle="Last 30 days; saved is cached reads at the input price less the cached price"
        bodyClassName="px-0 pb-0"
        className="xl:col-span-2"
      >
        {prefixes.length === 0 ? (
          <EmptyState
            size="sm"
            title="No provider prompt caching in the last 30 days"
            description="Turn it on for a model, or send cache markers from your app."
          />
        ) : (
          <div className="overflow-x-auto">
            <Table label="Provider prompt caching by prefix">
              <thead>
                <tr>
                  <Th title="A fingerprint of the system prompt, tools and first message">Prefix</Th>
                  <Th>Deployments</Th>
                  <Th numeric>Requests</Th>
                  <Th numeric>Cached reads</Th>
                  <Th numeric>Cache writes</Th>
                  <Th numeric>Saved</Th>
                </tr>
              </thead>
              <tbody>
                {shownPrefixes.map((p) => (
                  <Tr key={p.prefix}>
                    <Td mono title={p.prefix}>
                      {p.prefix.slice(0, 12)}…
                    </Td>
                    <Td mono muted>
                      {p.deployments.join(', ') || '—'}
                    </Td>
                    <Td numeric>{formatCount(p.requests)}</Td>
                    <Td numeric>{formatTokens(p.read)}</Td>
                    <Td numeric>{formatTokens(p.write)}</Td>
                    <Td numeric className={p.saved ? 'text-success' : undefined}>
                      {p.saved === null ? 'Not priced' : formatMoney(p.saved)}
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
            {prefixes.length > shownPrefixes.length ? (
              <p className="px-5 py-2.5 text-caption text-muted">
                And {plural(prefixes.length - shownPrefixes.length, 'more prefix', 'more prefixes')}.
              </p>
            ) : null}
          </div>
        )}
      </Panel>
      <Panel
        title="Overrides"
        subtitle="Organizations, teams, projects, keys and routes with their own cache settings"
        bodyClassName="px-0 pb-0"
        className="xl:col-span-2"
      >
        {overrides.length === 0 && !gateway ? (
          <EmptyState
            size="sm"
            title="Nothing overrides the cache settings"
            description="Every request uses its model’s settings."
          />
        ) : (
          <div className="overflow-x-auto">
            <Table label="Cache overrides">
              <thead>
                <tr>
                  <Th>Level</Th>
                  <Th>Name</Th>
                  <Th>Sets</Th>
                  <Th>The rest comes from</Th>
                </tr>
              </thead>
              <tbody>
                {gateway && Object.keys(gateway).length > 0 ? (
                  <Tr>
                    <Td>
                      <Badge tone="accent">Gateway</Badge>
                    </Td>
                    <Td muted>Default</Td>
                    <Td className="whitespace-normal">{describeCache(gateway).join(' · ') || '—'}</Td>
                    <Td muted>Built-in defaults</Td>
                  </Tr>
                ) : null}
                {overrides.map((o) => (
                  <Tr key={`${o.level}/${o.name}`}>
                    <Td>
                      <Badge tone="neutral">{o.level.charAt(0).toUpperCase() + o.level.slice(1)}</Badge>
                    </Td>
                    <Td mono>{o.name}</Td>
                    <Td className="whitespace-normal">{o.settings.join(' · ') || '—'}</Td>
                    <Td muted>{o.inherits.charAt(0).toUpperCase() + o.inherits.slice(1)}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
            <p className="px-5 py-2.5 text-caption text-muted">
              A model’s own settings win over all of these for the fields it sets.
            </p>
          </div>
        )}
      </Panel>
    </div>
  );
}
