'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Meter } from '@/components/ui/charts';
import { useCan } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { EmptyState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import type { UsageBucket } from '@/lib/api/types';
import { formatCount, formatPercent } from '@/lib/format';

import { describePredicate, isRecord } from '@/features/routing/types';

import { modelsApi } from '../api';
import { ruleOfPath } from '../catalog';
import { useRouteMap } from '../hooks';
import { conditionalRoot, describeNode, stepsOf } from '../tree';
import type { ModelView } from '../types';

const HOUR = 3_600_000;

/** Requests per tree position in the last 24 hours (`group_by=route_node`), or null while unknown. */
function useRouteNodeUsage(enabled: boolean): UsageBucket[] | null | 'error' {
  const [rows, setRows] = useState<UsageBucket[] | null | 'error'>(null);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const to = new Date();
    modelsApi
      .usageBy(new Date(to.getTime() - 24 * HOUR).toISOString(), to.toISOString(), ['route_node'])
      .then(
        (r) => live && setRows(r),
        () => live && setRows('error'),
      );
    return () => {
      live = false;
    };
  }, [enabled]);
  return rows;
}

interface RuleLine {
  key: string;
  name: string;
  condition: string;
  primary: string;
  fallbacks: string[];
  note: string;
  enabled: boolean;
  requests: number | null;
}

/** "a, then b, then c" of a rule's target: the first step and the ones tried after it. */
function chainOf(
  target: unknown,
  routes: ReturnType<typeof useRouteMap>,
): { primary: string; fallbacks: string[] } {
  if (isRecord(target) && target.type === 'fallback' && Array.isArray(target.children)) {
    const labels = (target.children as unknown[]).map((c) => describeNode(c, routes));
    return { primary: labels[0] ?? '—', fallbacks: labels.slice(1) };
  }
  return { primary: describeNode(target, routes), fallbacks: [] };
}

/**
 * The rule lists behind the catalog: for each Model whose tree starts with rules (its own or a shared
 * route's), its rules in order with the condition, target, fallbacks, note and share of the last 24
 * hours, linking to the Routing policy screen.
 */
export function RoutingTab({
  views,
  onOpenModel,
}: {
  views: ModelView[];
  onOpenModel: (name: string) => void;
}) {
  const routes = useRouteMap();
  const routingHref = useScopedHref('/routing/');
  const canUsage = useCan('getUsageAnalytics');
  const usage = useRouteNodeUsage(canUsage);
  const lists = views
    .map((v) => ({ view: v, root: conditionalRoot(v.name, v.model.spec, routes) }))
    .filter((x): x is { view: ModelView; root: NonNullable<typeof x.root> } => x.root !== null);
  const withChains = views.filter(
    (v) => !conditionalRoot(v.name, v.model.spec, routes) && stepsOf(v.model.spec).length > 1,
  );

  const linesOf = (route: string, tree: Record<string, unknown>): { lines: RuleLine[]; total: number } => {
    const rows = usage && usage !== 'error' ? usage : [];
    const counts = new Map<number | 'default', number>();
    let total = 0;
    for (const r of rows) {
      const path = r.group.route_node;
      if (!path) continue;
      const rule = ruleOfPath(path, route, typeof tree.id === 'string' ? tree.id : undefined);
      if (rule === null) continue;
      total += r.requests;
      counts.set(rule, (counts.get(rule) ?? 0) + r.requests);
    }
    const known = usage !== null && usage !== 'error';
    const rules = Array.isArray(tree.rules) ? (tree.rules as unknown[]) : [];
    const lines: RuleLine[] = rules.map((raw, i) => {
      const r = isRecord(raw) ? raw : {};
      return {
        key: `r${i}`,
        name: typeof r.name === 'string' ? r.name : `Rule ${i + 1}`,
        condition: describePredicate(r.when),
        ...chainOf(r.target, routes),
        note: typeof r.description === 'string' ? r.description : '',
        enabled: r.enabled !== false,
        requests: known ? (counts.get(i) ?? 0) : null,
      };
    });
    if (tree.default !== undefined)
      lines.push({
        key: 'default',
        name: 'Otherwise',
        condition: 'no rule matched',
        ...chainOf(tree.default, routes),
        note: '',
        enabled: true,
        requests: known ? (counts.get('default') ?? 0) : null,
      });
    return { lines, total };
  };

  return (
    <div className="flex flex-col">
      <p className="border-b border-line bg-raised px-5 py-3 text-ui text-muted max-sm:px-gutter-sm">
        Rules are tried in order and the first that matches sends the request; each target lists its
        fallbacks. Edit rules on the{' '}
        <Link href={routingHref} className="text-accent hover:underline">
          Routing policy
        </Link>{' '}
        screen.
      </p>
      {usage === 'error' ? (
        <p className="border-b border-line px-5 py-2 text-caption text-muted max-sm:px-gutter-sm">
          The share of the last 24 hours couldn’t be read.
        </p>
      ) : null}
      {lists.length === 0 ? (
        <EmptyState
          title="No rule lists"
          description="Point a model at a shared route with rules, or add rules on Routing policy."
        />
      ) : null}
      {lists.map(({ view, root }) => {
        const { lines, total } = linesOf(root.route, root.tree);
        return (
          <Section
            key={view.name}
            title={
              <button type="button" className="mono hover:text-accent" onClick={() => onOpenModel(view.name)}>
                {view.name}
              </button>
            }
            subtitle={
              root.shared
                ? `through the shared route ${root.route} · ${formatCount(total)} requests in 24 h`
                : `${formatCount(total)} requests in 24 h`
            }
            actions={
              root.shared ? (
                <Link
                  href={`${routingHref}?route=${encodeURIComponent(root.route)}`}
                  className="text-caption text-accent hover:underline"
                >
                  Open in Routing policy
                </Link>
              ) : null
            }
          >
            <div className="overflow-x-auto">
              <Table label={`Rules of ${view.name}`}>
                <thead>
                  <tr>
                    <Th>Rule</Th>
                    <Th>When</Th>
                    <Th>Sends to</Th>
                    <Th>Share, 24 h</Th>
                    <Th>Note</Th>
                  </tr>
                </thead>
                <tbody>
                  {lines.map((l) => (
                    <Tr key={l.key}>
                      <Td>
                        <span className="flex items-center gap-1.5">
                          <span className="mono">{l.name}</span>
                          {!l.enabled ? <Badge>Off</Badge> : null}
                        </span>
                      </Td>
                      <Td muted>{l.condition}</Td>
                      <Td>
                        <span className="mono text-fg">{l.primary}</span>
                        {l.fallbacks.map((f) => (
                          <span key={f} className="mono block text-caption text-muted">
                            ↳ {f}
                          </span>
                        ))}
                      </Td>
                      <Td>
                        {l.requests === null ? (
                          <span className="text-dim">—</span>
                        ) : (
                          <span className="flex min-w-32 items-center gap-2">
                            <Meter
                              value={l.requests}
                              max={Math.max(1, total)}
                              label={`${l.name}: ${formatCount(l.requests)} requests`}
                              className="flex-1"
                            />
                            <span className="mono w-12 text-right text-caption tabular-nums">
                              {total > 0 ? formatPercent(l.requests / total, 0) : '0%'}
                            </span>
                          </span>
                        )}
                      </Td>
                      <Td muted>{l.note || '—'}</Td>
                    </Tr>
                  ))}
                </tbody>
              </Table>
            </div>
          </Section>
        );
      })}
      {withChains.length > 0 ? (
        <Section title="Fallback chains" subtitle="models that try deployments in order">
          <ul className="flex flex-col gap-1.5 text-ui">
            {withChains.map((v) => {
              const steps = stepsOf(v.model.spec);
              return (
                <li key={v.name} className="flex flex-wrap items-center gap-x-2">
                  <button
                    type="button"
                    className="mono text-fg hover:text-accent"
                    onClick={() => onOpenModel(v.name)}
                  >
                    {v.name}
                  </button>
                  <span className="mono text-muted">
                    {steps.map((s) => describeNode(s.node, routes)).join(' → ')}
                  </span>
                </li>
              );
            })}
          </ul>
        </Section>
      ) : null}
    </div>
  );
}
