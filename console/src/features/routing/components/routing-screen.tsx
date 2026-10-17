'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { useEffect, useMemo, useRef, useState } from 'react';

import { useQueryParam, useScopedHref, useSettingsHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { FavouriteButton, useFavouriteNames } from '@/components/ui/favourite';
import { Input, Segmented } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { KpiStrip, KpiTile, type KpiState } from '@/components/ui/kpi';
import { FilterBar, PageHeader, Panel, SummaryStrip, Tabs } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { UpdatedAt } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import { Select } from '@/components/ui/select';
import {
  Banner,
  EmptyAction,
  EmptyState,
  ErrorState,
  isUnavailable,
  LoadingRows,
  NoMatch,
} from '@/components/ui/states';
import { CountNote } from '@/components/ui/table';
import { describeError, isChangeControl } from '@/lib/api/client';
import { downloadText } from '@/lib/csv';
import { formatCount, formatPercent, parseDuration, plural } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { formatClock } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useResolvedRange, useQueryWindow } from '@/stores/time-range';
import { notify } from '@/stores/ui';

import { useUsage, useUsageQueries } from '@/features/analytics/hooks';
import { totals, useAnalytics } from '@/features/analytics/store';
import { healthName, tierLabel } from '@/features/models/types';

import { routingApi, type RouteStateEntry } from '../api';
import {
  alertsOfRule,
  blocksByCategory,
  decisionLatency,
  type Editor,
  lastEditors,
  rates,
  resultsByRule,
  ruleImpact,
} from '../insights';
import { useRouting } from '../store';
import {
  afterDeletingRule,
  describePredicate,
  duplicateRule,
  isRecord,
  type Json,
  NODE_TYPES,
  type Rule,
  rulesOf,
  STRATEGIES,
  strategyOf,
  type Strategy,
  withRules,
} from '../types';
import { toYaml } from '../yaml';
import { ChangeFeed, isRoutingChange, PendingChanges } from './changes-tab';
import { PENDING } from '@/features/change-requests/api';
import { useChangeRequests } from '@/features/change-requests/store';
import { DefaultsPanel, HealthPanel, ResetSessionsDialog } from './health-defaults';
import { LiveDecisions } from './live-decisions';
import { RuleRow, strategyLabel, Targets } from './rule-row';
import { RuleWizard } from './rule-wizard';

type Tab = 'rules' | 'health' | 'live' | 'changes';

type RuleFilter = 'all' | 'enabled' | 'disabled' | 'alerting';
const RULE_STATES: RuleFilter[] = ['all', 'enabled', 'disabled', 'alerting'];

/** The rules list's filters as URL parameters, so a copied link keeps them. */
const RULE_FILTERS = { show: 'all', strategy: '', q: '' };
const TABS: Tab[] = ['rules', 'health', 'live', 'changes'];

/** Every node type a target uses, nested ones included. */
function nodeTypes(node: Json, out = new Set<string>()): Set<string> {
  if (typeof node === 'string') out.add('deployment');
  if (!isRecord(node)) return out;
  out.add(
    typeof node.type === 'string' ? node.type : typeof node.route === 'string' ? 'route' : 'deployment',
  );
  for (const key of ['children', 'rules', 'classes'] as const)
    if (Array.isArray(node[key]))
      for (const c of node[key] as Json[]) nodeTypes(isRecord(c) && 'target' in c ? c.target : c, out);
  for (const key of ['default', 'strong', 'weak'] as const)
    if (node[key] !== undefined) nodeTypes(node[key], out);
  return out;
}

function ReferenceDrawer({ open, onClose, rules }: { open: boolean; onClose: () => void; rules: Rule[] }) {
  const active = rules.filter((r) => r.enabled);
  return (
    <Drawer
      open={open}
      onClose={onClose}
      title="How routing works"
      subtitle="Each kind of target a rule can send to, and how many rules on use it"
    >
      <ul className="flex flex-col gap-4">
        {NODE_TYPES.map((n) => (
          <li key={n.type}>
            <div className="flex items-center gap-2">
              <span className="text-body font-semibold text-fg">{n.label}</span>
              <Badge>{plural(active.filter((r) => nodeTypes(r.target).has(n.type)).length, 'rule')}</Badge>
            </div>
            <p className="text-ui text-muted">{n.description}</p>
            {n.advanced ? (
              <p className="text-caption text-dim">{n.advanced} Set in the configuration.</p>
            ) : null}
          </li>
        ))}
      </ul>
    </Drawer>
  );
}

/**
 * Routing policy: the rule list of a shared route (its conditional root), checked from top to
 * bottom, with traffic figures, deployment health and defaults, live decisions and changes.
 */
export function RoutingScreen() {
  const routes = useRouting((s) => s.routes);
  const health = useRouting((s) => s.health);
  const tiers = useRouting((s) => s.tiers);
  const state = useRouting((s) => s.state);
  const reach = useRouting((s) => s.reach);
  const audit = useRouting((s) => s.audit);
  const alerts = useRouting((s) => s.alerts);
  const nodeUsage = useRouting((s) => s.nodeUsage);
  const categoryUsage = useRouting((s) => s.categoryUsage);
  const recent = useRouting((s) => s.recent);
  const load = useRouting((s) => s.load);
  const loadDetails = useRouting((s) => s.loadDetails);
  const loadState = useRouting((s) => s.loadState);
  const loadSettings = useRouting((s) => s.loadSettings);
  const saveTree = useRouting((s) => s.saveTree);
  const deleteRoute = useRouting((s) => s.deleteRoute);
  const canDeleteRoute = useCan('deleteRoute');
  const [removingRoute, setRemovingRoute] = useState<string | null>(null);
  const clearSimulation = useRouting((s) => s.clearSimulation);
  const canEdit = useCan('replaceRoute');
  const canReset = useCan('deleteRouteAffinity');
  const canExport = useCan('exportConfig');
  const canReadChanges = useCan('listChangeRequests');
  const loadChanges = useChangeRequests((s) => s.load);
  const router = useRouter();
  const timeZone = usePreferences((s) => s.timeZone);
  const auditHref = useSettingsHref('audit');
  const configHref = useSettingsHref('configuration');
  const [routeParam, setRouteParam] = useQueryParam('route');
  const [tabParam, setTabParam] = useQueryParam('tab');
  const url = useUrlFilters(RULE_FILTERS);
  const strategyParam = url.filters.strategy || null;
  // A rule's link (`rule=<name>`) opens its details once its route has loaded.
  const [ruleParam] = useQueryParam('rule');
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'rules';
  const starredRoutes = useFavouriteNames('route');
  const changesHref = useScopedHref('/change-requests/');
  const filter: RuleFilter = RULE_STATES.includes(url.filters.show as RuleFilter)
    ? (url.filters.show as RuleFilter)
    : 'all';
  const query = url.filters.q;
  const [wizard, setWizard] = useState<{ editing: Rule | null } | null>(null);
  const [deleting, setDeleting] = useState<Rule | null>(null);
  const [resetting, setResetting] = useState<string | null>(null);
  const [reference, setReference] = useState(false);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const range = useResolvedRange();
  const window = useQueryWindow();
  const { current, key } = useUsageQueries();
  const usageQueries = useMemo(
    () => ({
      r_total: { ...current, interval: undefined, groupBy: ['route' as const] },
      r_series: { ...current, groupBy: ['route' as const] },
    }),
    [current],
  );
  useUsage(usageQueries, key);
  // Requests routed are those whose record names this route, not every request of the range.
  const routeUsage = useAnalytics((s) => s.usage.r_total);
  const routeSeries = useAnalytics((s) => s.usage.r_series);

  useEffect(() => {
    void load();
    void loadDetails();
    void loadSettings();
    if (canReadChanges) void loadChanges({ quiet: true });
  }, [load, loadDetails, loadSettings, canReadChanges, loadChanges]);
  usePolling(() => {
    void load({ quiet: true });
    void loadDetails({ quiet: true });
  });

  const list = routes.data ?? [];
  const conditional = list.filter((r) => rulesOf(r.spec.tree) !== null);
  const route = list.find((r) => r.name === routeParam) ?? conditional[0] ?? list[0];
  const routeName = route?.name ?? null;
  useEffect(() => {
    if (routeName) void loadState(routeName);
  }, [routeName, loadState]);
  usePolling(() => {
    if (routeName) void loadState(routeName, { quiet: true });
  });

  const parsed = route ? rulesOf(route.spec.tree) : null;
  const rules = useMemo(() => parsed?.rules ?? [], [parsed]);
  const rootId =
    isRecord(route?.spec.tree) && typeof route.spec.tree.id === 'string' ? route.spec.tree.id : undefined;
  const rawRules = useMemo((): Json[] => {
    const tree: Json = route?.spec.tree;
    if (!isRecord(tree) || !Array.isArray(tree.rules)) return [];
    const out: Json[] = tree.rules;
    return out;
  }, [route]);
  const lastRule = deleting && route ? afterDeletingRule(route.spec.tree, rawRules, deleting.index) : null;

  const impact = useMemo(
    () => (route && nodeUsage.data ? ruleImpact(nodeUsage.data, route.name, rootId) : null),
    [route, nodeUsage.data, rootId],
  );
  const editors = useMemo(
    () => (route && audit.data ? lastEditors(audit.data, route.name) : new Map<string, Editor>()),
    [route, audit.data],
  );
  const results = useMemo(
    () =>
      route && state.data
        ? resultsByRule(state.data, route.name, rootId)
        : new Map<number | 'default', RouteStateEntry>(),
    [route, state.data, rootId],
  );
  const firing = useMemo(() => (alerts.data ?? []).filter((a) => a.status !== 'resolved'), [alerts.data]);
  const alertsByRule = useMemo(
    () => new Map(rules.map((r) => [r.index, route ? alertsOfRule(firing, route.name, r) : []])),
    [rules, firing, route],
  );

  const strategyFilter = STRATEGIES.some((s) => s.value === strategyParam)
    ? (strategyParam as Strategy)
    : null;
  const filtered = filter !== 'all' || query !== '' || strategyFilter !== null;
  const visible = rules.filter(
    (r) =>
      (filter === 'all' ||
        (filter === 'enabled'
          ? r.enabled
          : filter === 'disabled'
            ? !r.enabled
            : (alertsByRule.get(r.index)?.length ?? 0) > 0)) &&
      (strategyFilter === null || strategyOf(r.target) === strategyFilter) &&
      (!query ||
        `${r.name} ${r.description} ${r.tags.join(' ')} ${describePredicate(r.when)}`
          .toLowerCase()
          .includes(query.toLowerCase())),
  );
  const usage = routeUsage
    ? { ...routeUsage, data: routeUsage.data?.filter((b) => b.group.route === route?.name) ?? null }
    : routeUsage;
  const series = routeSeries
    ? { ...routeSeries, data: routeSeries.data?.filter((b) => b.group.route === route?.name) ?? null }
    : routeSeries;
  const t = totals(usage?.data);
  const deployments = health.data ?? [];
  const open = deployments.filter((h) => h.state !== 'closed');
  const buckets = series?.data ?? [];
  const active = rules.filter((r) => r.enabled).length;
  const changeRequests = useChangeRequests((s) => s.list);
  const pendingRouting = changeRequests.data
    ? changeRequests.data.filter((cr) => PENDING.includes(cr.state) && isRoutingChange(cr)).length
    : 0;

  const usageState: KpiState = usage?.error
    ? isUnavailable(usage.error)
      ? 'unavailable'
      : 'empty'
    : !usage?.data
      ? 'loading'
      : (t?.requests ?? 0) === 0
        ? 'empty'
        : 'ready';

  const rate = rates(buckets, parseDuration(window.interval) ?? 3600);
  const decision = decisionLatency(recent.data ?? []);
  const categories = blocksByCategory(categoryUsage.data ?? []);
  const armed = (state.data ?? []).filter((e) => e.canary?.state === 'ramping').length;
  const stopped = rules.flatMap((rule) => {
    const entry = results.get(rule.index);
    return entry?.canary && (entry.canary.state === 'rolled_back' || entry.canary.state === 'held')
      ? [{ rule, entry }]
      : [];
  });

  async function commit(nextRules: Json[], message: string, detail?: string) {
    if (!route) return;
    try {
      await saveTree(route.name, withRules(route.spec.tree, nextRules));
      notify('success', message, detail);
    } catch (e) {
      // Change control: the shell offers or shows the change request.
      if (isChangeControl(e)) return;
      notify('error', 'Couldn’t save the rule', describeError(e));
    }
  }

  function move(from: number, to: number) {
    if (from === to || to < 0 || to >= rawRules.length) return;
    const next = [...rawRules];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    const name = rules[from]?.name ?? 'Rule';
    void commit(next, 'Rule moved', `${name} is now number ${to + 1}.`);
  }

  async function exportYaml() {
    try {
      const config = (await routingApi.config()) as Record<string, unknown>;
      const doc = { routes: config.routes ?? {}, models: config.models ?? {} };
      downloadText('routing.yaml', toYaml(doc), 'application/yaml');
      notify('success', 'Routing exported', 'Routes and Models as YAML.');
    } catch (e) {
      notify('error', 'Couldn’t export routing', describeError(e));
    }
  }

  const setTab = (next: Tab) => setTabParam(next === 'rules' ? null : next);
  const canCreate = canEdit && (parsed !== null || (routes.data !== null && !route));
  usePageShortcut('n', () => {
    if (canCreate) setWizard({ editing: null });
  });
  usePageShortcut('e', () => {
    if (canExport) void exportYaml();
  });

  const more: MenuItem[] = [
    { label: 'How routing works', icon: 'info', onSelect: () => setReference(true) },
    ...(canExport && list.length > 0
      ? [{ label: 'Export YAML', icon: 'download' as const, onSelect: () => void exportYaml() }]
      : []),
    ...(route && canDeleteRoute
      ? [
          {
            label: `Delete route ${route.name}`,
            icon: 'trash' as const,
            danger: true,
            onSelect: () => setRemovingRoute(route.name),
          },
        ]
      : []),
    ...(route && canReset
      ? [
          {
            label: `Reset sessions of ${route.name}`,
            icon: 'refresh' as const,
            onSelect: () => setResetting(route.name),
          },
        ]
      : []),
    { label: 'View changes in the audit log', icon: 'clock', onSelect: () => router.push(auditHref) },
  ];

  const linkedRule = ruleParam ? rules.find((r) => r.name === ruleParam) : undefined;
  const linkedIndex = linkedRule?.index ?? null;
  const shownLink = useRef<string | null>(null);
  useEffect(() => {
    if (linkedIndex === null || !routeName) return;
    const at = `${routeName}/${linkedIndex}`;
    if (shownLink.current === at) return;
    shownLink.current = at;
    setExpanded((prev) => new Set(prev).add(linkedIndex));
    requestAnimationFrame(() =>
      document.getElementById(`rule-${linkedIndex}-detail`)?.scrollIntoView({ block: 'center' }),
    );
  }, [linkedIndex, routeName]);
  /** Copies the link that opens this route on one rule, with its details open. */
  const copyRuleLink = (r: Rule) => {
    const link = new URL(document.location.href);
    link.searchParams.delete('tab');
    if (routeName) link.searchParams.set('route', routeName);
    link.searchParams.set('rule', r.name);
    void navigator.clipboard
      .writeText(link.toString())
      .then(() => notify('success', 'Link copied', `Rule ${r.name}`));
  };

  const toggleExpanded = (i: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  return (
    <>
      <PageHeader
        title="Routing policy"
        description="Send requests to the right deployments by team, tag or size"
        meta={
          parsed ? (
            <>
              {plural(active, 'active rule')}
              {pendingRouting ? (
                <>
                  {' · '}
                  <Link href={changesHref} className="text-warn hover:underline">
                    {plural(pendingRouting, 'pending change')}
                  </Link>
                </>
              ) : null}
            </>
          ) : undefined
        }
        context={range.label}
        actions={
          <div className="flex flex-wrap items-center gap-2 max-sm:min-w-[calc(100vw-32px)]">
            <UpdatedAt at={routes.at} />
            {list.length > 1 ? (
              <div className="w-44">
                <Select
                  aria-label="Route"
                  value={route?.name ?? ''}
                  onChange={(v) => setRouteParam(v)}
                  options={[...list]
                    .sort((a, b) => Number(starredRoutes.has(b.name)) - Number(starredRoutes.has(a.name)))
                    .map((r) => ({
                      value: r.name,
                      label: `Route ${r.name}${starredRoutes.has(r.name) ? ' (starred)' : ''}`,
                    }))}
                  className="h-8"
                />
              </div>
            ) : null}
            {/* With one route the page's own star is enough; with several, the chosen route has its own. */}
            {route && list.length > 1 ? (
              <FavouriteButton kind="route" name={route.name} label={`route ${route.name}`} />
            ) : null}
            <MenuButton items={more} label="More" variant="ghost" />
            {canCreate ? (
              <Button variant="primary" icon="plus" onClick={() => setWizard({ editing: null })}>
                Create rule
              </Button>
            ) : null}
          </div>
        }
      />
      {route ? (
        <>
          <KpiStrip columns={4} label="Routing figures">
            <KpiTile
              label="Requests routed"
              icon="route"
              state={usageState}
              emptyHint="In this time range, none yet"
              value={formatCount(t?.requests ?? 0, { compact: true })}
              context={
                reach?.models?.length === 0
                  ? 'No Model sends requests to this route'
                  : active === 0
                    ? 'No rule on: all went to the default'
                    : `Through route ${route.name}, ${plural(active, 'rule')} on`
              }
              spark={{ values: buckets.map((b) => b.requests) }}
            />
            <KpiTile
              label="Fell back to another deployment"
              icon="refresh"
              state={usageState}
              emptyHint="In this time range, none yet"
              value={formatCount(t?.fallbacks ?? 0, { compact: true })}
              context={`${formatPercent((t?.fallbacks ?? 0) / Math.max(1, t?.requests ?? 0), 1)} of requests`}
              spark={{ values: buckets.map((b) => b.fallbacks), tone: 'warn' }}
            />
            <KpiTile
              label="Blocked or flagged by guardrails"
              icon="shield"
              state={usageState}
              emptyHint="In this time range, none yet"
              value={formatCount(t?.flags ?? 0, { compact: true })}
              valueTone={(t?.flags ?? 0) > 0 ? 'warn' : 'neutral'}
              context="Includes block rules"
              spark={{
                values: buckets.map((b) => (b.guardrail_blocks ?? 0) + (b.guardrail_flags ?? 0)),
                tone: 'warn',
              }}
            />
            <KpiTile
              label="Deployments paused after errors"
              icon="heart"
              state={
                health.error
                  ? 'unavailable'
                  : !health.data
                    ? 'loading'
                    : deployments.length === 0
                      ? 'empty'
                      : 'ready'
              }
              emptyHint="No traffic yet"
              value={`${open.length} of ${deployments.length}`}
              valueTone={open.length > 0 ? 'error' : 'neutral'}
              context={open.length === 0 ? 'All deployments healthy' : 'Requests go around them'}
              spark={null}
            />
          </KpiStrip>
          {usageState === 'ready' ? (
            <SummaryStrip
              label="Routing details"
              items={[
                {
                  label: 'Requests per second',
                  value: rate ? rate.current.toFixed(rate.current < 10 ? 2 : 0) : '—',
                  sub: rate?.peakAt
                    ? `peak ${rate.peak.toFixed(rate.peak < 10 ? 2 : 0)}/s at ${formatClock(rate.peakAt, timeZone)}`
                    : undefined,
                  title: 'In the last bucket of the chart, and the busiest bucket of the window',
                },
                {
                  label: 'Decision time p95',
                  value: decision.p95 === null ? '—' : `${decision.p95.toFixed(2)} ms`,
                  sub:
                    decision.p99 === null
                      ? 'Once requests report stage timing'
                      : `p99 ${decision.p99.toFixed(2)} ms · last ${plural(decision.count, 'request')}`,
                  title:
                    'Time spent resolving the model and choosing a deployment, from the last hour’s records',
                },
                {
                  label: 'Fallback share',
                  value: formatPercent((t?.fallbacks ?? 0) / Math.max(1, t?.requests ?? 0), 1),
                  sub: `${formatCount(t?.fallbacks ?? 0)} of ${formatCount(t?.requests ?? 0)} requests`,
                },
                {
                  label: 'Blocks, last 24 h',
                  value: formatCount(categories.reduce((n, c) => n + c.blocks, 0)),
                  tone: categories.length > 0 ? 'warn' : 'neutral',
                  sub:
                    categories.length > 0
                      ? categories
                          .slice(0, 3)
                          .map((c) => `${c.category} ${formatCount(c.blocks)}`)
                          .join(' · ')
                      : 'None',
                  title: 'Guardrail and routing blocks by category',
                },
                ...(armed > 0
                  ? [
                      {
                        label: 'Canaries',
                        value: `${armed} ramping`,
                        tone: 'warn' as const,
                        sub: 'Auto-rollback armed',
                      },
                    ]
                  : []),
              ]}
            />
          ) : null}
        </>
      ) : null}
      {open.map((h) => (
        <Banner key={healthName(h)} tone="error" title={`${healthName(h)} is paused after errors`}>
          {plural(h.window.failures, 'failure')} in its last {plural(h.window.requests, 'request')}; it has
          paused {plural(h.trips, 'time')}. Requests go to the next deployment until it recovers.
        </Banner>
      ))}
      {stopped.map(({ rule, entry }) => (
        <Banner
          key={entry.path}
          tone="warn"
          title={`Canary of ${rule.name} ${entry.canary?.state === 'held' ? 'held' : 'rolled back'}`}
        >
          The candidate {entry.canary?.child ?? ''} passed its error budget
          {entry.canary?.error_rate != null ? ` at ${formatPercent(entry.canary.error_rate, 1)} errors` : ''},
          so it gets no traffic. Saving the rule again restarts the ramp.
        </Banner>
      ))}
      {routes.status === 'loading' && !routes.data ? <LoadingRows /> : null}
      {routes.error ? <ErrorState error={routes.error} subject="Routes" onRetry={() => void load()} /> : null}
      {routes.data && !route ? (
        <EmptyState
          title="No routing rules yet"
          description="Rules send matching requests to the deployments you choose."
          action={
            <Gate operation="replaceRoute" mode="hide">
              <EmptyAction onClick={() => setWizard({ editing: null })}>Create rule</EmptyAction>
            </Gate>
          }
          secondaryAction={<EmptyAction onClick={() => setReference(true)}>How routing works</EmptyAction>}
        />
      ) : null}
      {route ? (
        <Tabs<Tab>
          label="Routing views"
          value={tab}
          onChange={setTab}
          items={[
            { value: 'rules', label: 'Rules', count: rules.length, icon: 'route' },
            { value: 'health', label: 'Health and defaults', icon: 'heart' },
            { value: 'live', label: 'Live decisions', icon: 'activity' },
            {
              value: 'changes',
              label: 'Changes',
              icon: 'history',
              ...(pendingRouting ? { count: pendingRouting } : {}),
            },
          ]}
        />
      ) : null}
      {route && tab === 'rules' && !parsed ? (
        <EmptyState
          title={`Route ${route.name} has no rule list`}
          description={`It sends every request with one strategy (${strategyLabel(route.spec.tree).toLowerCase()}). Edit it in the configuration to add rules.`}
        />
      ) : null}
      {tab === 'rules' && parsed ? (
        <section aria-labelledby="rules-title">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line bg-raised px-gutter pt-4 pb-3 max-sm:px-gutter-sm">
            <h2 id="rules-title" className="text-title font-semibold text-fg">
              Rules
            </h2>
            <span className="text-ui text-muted">
              Checked from top to bottom; the first match wins. Shares are of the last 24 hours.
            </span>
          </div>
          <FilterBar>
            <Input
              data-page-search
              aria-label="Search rules"
              icon="search"
              value={query}
              onChange={(e) => url.set({ q: e.target.value })}
              placeholder="Search rules"
              className="h-7 w-60"
            />
            <Segmented<'all' | 'enabled' | 'disabled' | 'alerting'>
              label="Rule status"
              value={filter}
              onChange={(show) => url.set({ show })}
              options={[
                { value: 'all', label: 'All', count: rules.length },
                { value: 'enabled', label: 'On', count: active },
                { value: 'disabled', label: 'Off', count: rules.length - active },
                {
                  value: 'alerting',
                  label: 'Alerting',
                  count: rules.filter((r) => (alertsByRule.get(r.index)?.length ?? 0) > 0).length,
                },
              ]}
            />
            <div className="w-48">
              <Select
                aria-label="Strategy"
                value={strategyFilter ?? ''}
                onChange={(v) => url.set({ strategy: v })}
                options={[
                  { value: '', label: 'Every strategy' },
                  ...STRATEGIES.map((s) => ({
                    value: s.value,
                    label: `${s.label} (${rules.filter((r) => strategyOf(r.target) === s.value).length})`,
                  })),
                ]}
                className="h-7"
              />
            </div>
            <span className="ml-auto whitespace-nowrap">
              <CountNote shown={visible.length} total={rules.length} noun="rules" />
            </span>
          </FilterBar>
          {visible.length === 0 && rules.length > 0 ? <NoMatch onClear={() => url.clear()} /> : null}
          <ol aria-label="Rules in the order they are checked">
            {visible.map((r) => (
              <RuleRow
                key={`${r.index}-${r.name}`}
                rule={r}
                position={r.index + 1}
                count={rules.length}
                canEdit={canEdit}
                canReorder={!filtered}
                dragging={dragIndex === r.index}
                expanded={expanded.has(r.index)}
                impact={impact ? (impact.byRule.get(r.index) ?? null) : null}
                total={impact ? impact.total : null}
                editor={editors.get(r.name) ?? null}
                alerts={alertsByRule.get(r.index) ?? []}
                result={results.get(r.index)}
                configHref={configHref}
                onExpand={() => toggleExpanded(r.index)}
                onCopyLink={() => copyRuleLink(r)}
                onEdit={() => setWizard({ editing: r })}
                onToggle={(on) =>
                  void commit(
                    rawRules.map((x, i) => (i === r.index && isRecord(x) ? { ...x, enabled: on } : x)),
                    on ? 'Rule turned on' : 'Rule turned off',
                    r.name,
                  )
                }
                onMove={(delta) => move(r.index, r.index + delta)}
                onDuplicate={() =>
                  void commit(
                    duplicateRule(rawRules, r.index),
                    'Rule duplicated',
                    `The copy of ${r.name} is under it, turned off.`,
                  )
                }
                onDelete={() => setDeleting(r)}
                onDragStart={() => setDragIndex(r.index)}
                onDrop={() => {
                  if (dragIndex !== null) move(dragIndex, r.index);
                  setDragIndex(null);
                }}
              />
            ))}
            <li className="grid grid-cols-[28px_20px_minmax(0,1fr)_150px] items-center gap-3 border-b border-line bg-canvas px-gutter py-3 max-lg:grid-cols-[28px_20px_minmax(0,1fr)] max-sm:grid-cols-[28px_minmax(0,1fr)] max-sm:px-gutter-sm">
              <span className="text-center text-muted">
                <Icon name="arrowRight" size={16} />
              </span>
              <span className="max-sm:hidden" />
              <p className="text-ui leading-6 text-muted">
                <span className="font-semibold text-fg">Everything else</span>
                {' → '}
                {reach?.models?.length === 0 ? (
                  <span>
                    no Model sends requests to this route, so its rules see no traffic; each Model routes on
                    its own
                  </span>
                ) : parsed.fallback === undefined && reach?.unmatched !== 'default' ? (
                  <span>
                    <span className="text-error">answers with an error</span> (no rule matched)
                    {reach?.models?.length ? (
                      <> for requests to {reach.models.join(', ')}; other Models never reach this route</>
                    ) : null}
                  </span>
                ) : parsed.fallback !== undefined ? (
                  <Targets node={parsed.fallback} />
                ) : (
                  <span>the route&apos;s default</span>
                )}
              </p>
              <span className="mono text-caption text-muted tabular-nums max-lg:hidden">
                {impact && impact.total > 0
                  ? `${formatPercent((impact.byRule.get('default')?.requests ?? 0) / impact.total, 1)} · ${formatCount(impact.byRule.get('default')?.requests ?? 0, { compact: true })} / 24h`
                  : ''}
              </span>
            </li>
          </ol>
          {(tiers.data?.length ?? 0) > 0 ? (
            <Panel
              title="Tiers"
              subtitle="Groups of models a rule can send to by tier"
              className="border-b border-line"
            >
              <ul className="flex flex-col">
                {(tiers.data ?? []).map((tier) => (
                  <li
                    key={tier.name}
                    className="flex items-baseline gap-3 border-b border-line py-2 text-ui last:border-0"
                  >
                    <span className="w-28 shrink-0 font-semibold text-fg">{tierLabel(tier.name)}</span>
                    <span className="min-w-0 flex-1 truncate text-muted">{tier.description ?? ''}</span>
                    <span className="shrink-0 text-caption text-muted">{plural(tier.models, 'model')}</span>
                  </li>
                ))}
              </ul>
            </Panel>
          ) : null}
        </section>
      ) : null}
      {route && tab === 'health' ? (
        <div className="grid gap-px border-b border-line bg-line lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
          <HealthPanel route={route.name} state={state.data} onReset={() => setResetting(route.name)} />
          <DefaultsPanel />
        </div>
      ) : null}
      {route && tab === 'live' ? <LiveDecisions route={route.name} rules={rules} /> : null}
      {route && tab === 'changes' ? (
        <div className="grid gap-px border-b border-line bg-line lg:grid-cols-2">
          <PendingChanges />
          <ChangeFeed route={route.name} rules={rules} />
        </div>
      ) : null}
      {wizard && (route || routes.data) ? (
        <RuleWizard
          route={route ? route.name : null}
          tree={route?.spec.tree ?? null}
          rules={rawRules}
          editing={wizard.editing}
          onClose={() => {
            clearSimulation();
            setWizard(null);
          }}
        />
      ) : null}
      <ReferenceDrawer open={reference} onClose={() => setReference(false)} rules={rules} />
      <ResetSessionsDialog route={resetting} onClose={() => setResetting(null)} />
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete rule ${deleting?.name ?? ''}?`}
        objectName={deleting?.name}
        typeToConfirm={deleting?.name}
        confirmLabel="Delete rule"
        effect={
          lastRule?.kind === 'route'
            ? `It is the only rule of route ${route?.name ?? ''}, which has no other target, so the route is deleted with it. Models that use the route must be pointed elsewhere first.`
            : lastRule?.kind === 'default'
              ? `It is the last rule of route ${route?.name ?? ''}: every request goes to the route's other target.`
              : 'Requests it matched go on to the next rule, then to everything else.'
        }
        onConfirm={async () => {
          if (!deleting || !route || !lastRule) return;
          try {
            if (lastRule.kind === 'route') {
              await deleteRoute(route.name);
              setRouteParam(null);
              notify('success', 'Rule and route deleted', `${deleting.name} · ${route.name}`);
            } else {
              await saveTree(route.name, lastRule.tree);
              notify('success', 'Rule deleted', deleting.name);
            }
          } catch (e) {
            if (isChangeControl(e)) return;
            throw e;
          }
        }}
      />
      <ConfirmDialog
        open={removingRoute !== null}
        onClose={() => setRemovingRoute(null)}
        title={`Delete route ${removingRoute ?? ''}?`}
        objectName={removingRoute ?? undefined}
        typeToConfirm={removingRoute ?? undefined}
        confirmLabel="Delete route"
        effect="Its rules go with it. A Model that uses the route must be pointed elsewhere first; the gateway refuses the delete until then."
        onConfirm={async () => {
          if (!removingRoute) return;
          try {
            await deleteRoute(removingRoute);
            setRouteParam(null);
            notify('success', 'Route deleted', removingRoute);
          } catch (e) {
            if (isChangeControl(e)) return;
            throw e;
          }
        }}
      />
    </>
  );
}
