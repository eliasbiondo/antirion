'use client';

import { Fragment, useMemo, useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Input, Segmented, Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { FilterBar } from '@/components/ui/layout';
import { ModelMark } from '@/components/ui/marks';
import { EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote, Table, Td, Th, Tr } from '@/components/ui/table';
import type { ModelResource, RouteResource } from '@/lib/api/types';
import { formatCount, formatMoney, formatPercent, plural } from '@/lib/format';
import { notify } from '@/stores/ui';

import {
  cacheOf,
  cachedReadDiscount,
  deploymentsOf,
  duration,
  keyStrategyOf,
  modesOf,
  size,
  type ModelUsage,
} from '../policy';
import { useCache } from '../store';
import { ClearDialog } from './clear-dialog';
import { SettingsDrawer } from './settings-drawer';

export type ModeFilter = 'all' | 'exact' | 'semantic' | 'prompt' | 'off';

/** The Model's operator note: its `note` annotation, else every annotation as "key: value". */
function noteOf(m: ModelResource): string | null {
  const a = m.spec.annotations;
  if (!a || Object.keys(a).length === 0) return null;
  if (a.note) return a.note;
  return Object.entries(a)
    .map(([k, v]) => `${k}: ${v}`)
    .join(' · ');
}

function percentRange(r: { min: number; max: number }): string {
  const lo = Math.round(r.min * 100);
  const hi = Math.round(r.max * 100);
  return lo === hi ? `${lo}%` : `${lo}–${hi}%`;
}

function Fact({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <>
      <dt className="text-caption text-muted">{label}</dt>
      <dd className="min-w-0 truncate text-ui text-fg">{value}</dd>
    </>
  );
}

/** What a Model's caching does, its settings and its 30 days, under its row. */
function ModelDetail({
  model,
  usage,
  onConfigure,
  onClear,
}: {
  model: ModelResource;
  usage: ModelUsage | undefined;
  onConfigure: () => void;
  onClear: () => void;
}) {
  const routes = useCache((s) => s.routes.data);
  const prices = useCache((s) => s.prices.data);
  const c = cacheOf(model);
  const modes = modesOf(c);
  const note = noteOf(model);
  const discount = prices
    ? cachedReadDiscount(deploymentsOf(model, routes ?? new Map<string, RouteResource>()), prices)
    : null;
  const lines: string[] = [];
  if (modes.exact)
    lines.push(
      modes.semantic
        ? `Repeated and similar prompts (similarity ${(c.semantic?.threshold ?? 0.95).toFixed(2)} or more, compared with ${c.semantic?.embedding_model ?? 'its embedding model'}) are answered from the cache.`
        : 'Requests identical to an earlier one are answered from the cache.',
    );
  if (modes.prompt)
    lines.push(
      `Long prompt prefixes are marked so the provider bills them at its cached rate${
        discount ? ` (${percentRange(discount)} off input)` : ''
      }.`,
    );
  if (lines.length === 0) lines.push('Caching is off. Every request goes to the provider.');
  const rate = usage && usage.requests > 0 ? usage.hits / usage.requests : null;
  return (
    <div className="grid gap-6 bg-canvas px-gutter py-4 max-sm:px-gutter-sm lg:grid-cols-3">
      <section aria-label="What this caches" className="flex flex-col gap-2">
        <h3 className="text-caption font-semibold text-muted">What this caches</h3>
        {lines.map((l) => (
          <p key={l} className="text-ui text-fg">
            {l}
          </p>
        ))}
        {note ? <p className="border-l-2 border-line-strong pl-2.5 text-ui text-muted">{note}</p> : null}
      </section>
      <section aria-label="Settings" className="flex flex-col gap-2">
        <h3 className="text-caption font-semibold text-muted">Settings</h3>
        <dl className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)] gap-x-3 gap-y-1.5">
          <Fact label="Keep responses" value={modes.exact ? duration(c.ttl ?? '1h') : '—'} />
          <Fact label="Same request when" value={keyStrategyOf(c).label} />
          <Fact label="Largest response" value={size(c.max_entry_size)} />
          <Fact label="Most responses" value={c.max_entries ? formatCount(c.max_entries) : 'No cap'} />
          <Fact label="Read only" value={c.read_only === true ? 'Yes, no new responses stored' : 'No'} />
          <Fact
            label="If the cache is down"
            value={c.fail_mode === 'closed' ? 'Refuse requests' : 'Send to the provider'}
          />
          <Fact
            label="Embeddings"
            value={c.embeddings?.per_item === true ? 'Cached per input' : 'Cached per request'}
          />
          <Fact
            label="Cached-read discount"
            value={
              discount ? (
                <span className="text-success">{percentRange(discount)} off input</span>
              ) : prices ? (
                'Not priced'
              ) : (
                '—'
              )
            }
          />
        </dl>
        <div className="mt-2 flex flex-wrap gap-2">
          <Gate operation="patchModel" mode="hide">
            <Button size="sm" variant="secondary" icon="settings" onClick={onConfigure}>
              Settings
            </Button>
          </Gate>
          {modes.exact ? (
            <Gate operation="invalidateCache" mode="hide">
              <Button size="sm" variant="ghost" icon="trash" onClick={onClear}>
                Clear this model’s cache
              </Button>
            </Gate>
          ) : null}
        </div>
      </section>
      <section aria-label="Last 30 days" className="flex flex-col gap-2">
        <h3 className="text-caption font-semibold text-muted">Last 30 days</h3>
        {usage ? (
          <dl className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)] gap-x-3 gap-y-1.5">
            <Fact label="Requests" value={formatCount(usage.requests)} />
            <Fact
              label="From the cache"
              value={`${formatCount(usage.hits)}${rate !== null ? ` (${formatPercent(rate, 0)})` : ''}`}
            />
            <Fact label="Saved" value={<span className="text-success">{formatMoney(usage.saved)}</span>} />
            <Fact label="Paid to providers" value={formatMoney(usage.cost)} />
            <Fact label="Without the cache" value={formatMoney(usage.cost + usage.saved)} />
          </dl>
        ) : (
          <EmptyState size="sm" title="No requests in the last 30 days" />
        )}
        {modes.exact && rate !== null && usage && usage.requests >= 50 && rate < 0.05 ? (
          <p className="text-caption text-warn">
            Few repeats: under 5% of requests hit the cache. A shorter key or a longer lifetime may help.
          </p>
        ) : null}
      </section>
    </div>
  );
}

/** Caching per Model: three independent switches, settings, 30-day figures and details. */
export function ModelsTab() {
  const models = useCache((s) => s.models);
  const byModel = useCache((s) => s.byModel);
  const load = useCache((s) => s.load);
  const setModelCache = useCache((s) => s.setModelCache);
  const canEdit = useCan('patchModel');
  const modelsHref = useScopedHref('/models/');
  const [filter, setFilter] = useState<ModeFilter>('all');
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [configuring, setConfiguring] = useState<ModelResource | null>(null);
  const [clearing, setClearing] = useState<string | null>(null);
  const [turningOn, setTurningOn] = useState(false);

  const list = useMemo(() => models.data ?? [], [models.data]);
  const counts = useMemo(() => {
    const out = { exact: 0, semantic: 0, prompt: 0, off: 0 };
    for (const m of list) {
      const md = modesOf(cacheOf(m));
      if (md.exact && !md.semantic) out.exact += 1;
      if (md.semantic) out.semantic += 1;
      if (md.prompt) out.prompt += 1;
      if (!md.exact && !md.prompt) out.off += 1;
    }
    return out;
  }, [list]);
  const offList = list.filter((m) => !modesOf(cacheOf(m)).exact);
  const visible = useMemo(
    () =>
      list.filter((m) => {
        const md = modesOf(cacheOf(m));
        if (filter === 'off' && (md.exact || md.prompt)) return false;
        if (filter === 'exact' && (!md.exact || md.semantic)) return false;
        if (filter === 'semantic' && !md.semantic) return false;
        if (filter === 'prompt' && !md.prompt) return false;
        const q = query.trim().toLowerCase();
        return (
          !q || m.name.toLowerCase().includes(q) || (m.spec.display_name ?? '').toLowerCase().includes(q)
        );
      }),
    [list, filter, query],
  );

  const toggle = (name: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  const setMode = (
    m: ModelResource,
    patch: Record<string, unknown>,
    done: string,
    undo: Record<string, unknown>,
  ) =>
    setModelCache(m.name, patch).then(
      () =>
        notify('success', done, m.name, {
          label: 'Undo',
          onClick: () => void setModelCache(m.name, undo),
        }),
      () => undefined,
    );

  return (
    <section aria-labelledby="cache-models-title">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-line bg-raised px-gutter pt-4 pb-3 max-sm:px-gutter-sm">
        <h2 id="cache-models-title" className="text-title font-semibold text-fg">
          Models
        </h2>
        <span className="text-ui text-muted">
          A model’s settings win over its teams’ and keys’; what it leaves unset comes from them.
        </span>
        {canEdit && offList.length > 0 && list.length > 0 ? (
          <Button size="sm" className="ml-auto" icon="check" onClick={() => setTurningOn(true)}>
            Turn on for all ({offList.length})
          </Button>
        ) : null}
      </div>
      {list.length > 0 ? (
        <FilterBar>
          <Input
            data-page-search
            aria-label="Search models"
            icon="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search models"
            className="h-7 w-52"
          />
          <Segmented<ModeFilter>
            label="Caching"
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: 'All', count: list.length },
              { value: 'exact', label: 'Exact', count: counts.exact },
              { value: 'semantic', label: 'Semantic', count: counts.semantic },
              { value: 'prompt', label: 'Prompt', count: counts.prompt },
              { value: 'off', label: 'Off', count: counts.off },
            ]}
          />
          <span className="ml-auto whitespace-nowrap">
            <CountNote shown={visible.length} total={list.length} noun="models" />
          </span>
        </FilterBar>
      ) : null}
      {models.status === 'loading' && !models.data ? <LoadingRows /> : null}
      {models.error ? <ErrorState error={models.error} subject="Models" onRetry={() => void load()} /> : null}
      {models.data?.length === 0 ? (
        <EmptyState title="No models yet" action={<EmptyAction href={modelsHref}>Add model</EmptyAction>} />
      ) : null}
      {list.length > 0 && visible.length === 0 ? (
        <NoMatch
          onClear={() => {
            setFilter('all');
            setQuery('');
          }}
        />
      ) : null}
      {visible.length > 0 ? (
        <div className="overflow-x-auto border-b border-line bg-raised">
          <Table label="Caching per model">
            <thead>
              <tr>
                <Th>
                  <span className="sr-only">Response caching on</span>
                </Th>
                <Th>Model</Th>
                <Th>Response cache</Th>
                <Th title="The provider bills marked prompt prefixes at its cached rate">Prompt caching</Th>
                <Th>Hit rate, 30 days</Th>
                <Th numeric>Saved, 30 days</Th>
                <Th>Settings</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {visible.map((m) => {
                const c = cacheOf(m);
                const md = modesOf(c);
                const u = byModel.data?.get(m.name);
                const rate = u && u.requests > 0 ? u.hits / u.requests : null;
                const open = expanded.has(m.name);
                return (
                  <Fragment key={m.name}>
                    <Tr>
                      <Td className="w-12">
                        <Switch
                          label={`${md.exact ? 'Turn off' : 'Turn on'} response caching for ${m.name}`}
                          hideLabel
                          checked={md.exact}
                          disabled={!canEdit}
                          disabledReason="You don't have permission to change models."
                          onChange={(next) =>
                            void setMode(
                              m,
                              { enabled: next },
                              next ? 'Caching turned on' : 'Caching turned off',
                              { enabled: !next },
                            )
                          }
                        />
                      </Td>
                      <Td>
                        <button
                          type="button"
                          className="flex items-start gap-2 text-left"
                          aria-expanded={open}
                          aria-controls={`cache-model-${m.name}`}
                          onClick={() => toggle(m.name)}
                        >
                          <ModelMark
                            model={m.name}
                            ownedBy={m.spec.owned_by}
                            family={m.spec.catalog?.family}
                            className="mt-0.5 text-muted"
                          />
                          <div>
                            <div className="text-body text-fg hover:underline">
                              {m.spec.display_name ?? m.name}
                            </div>
                            {/* The id under the name only when it says something the name doesn't. */}
                            {m.spec.display_name && m.spec.display_name !== m.name ? (
                              <div className="mono text-caption text-muted">{m.name}</div>
                            ) : null}
                          </div>
                        </button>
                      </Td>
                      <Td>
                        <span className="flex gap-1">
                          {md.exact ? (
                            <Badge tone={md.semantic ? 'info' : 'accent'}>
                              {md.semantic ? 'Semantic' : 'Exact'}
                            </Badge>
                          ) : (
                            <span className="text-muted">Off</span>
                          )}
                          {md.exact && c.read_only === true ? <Badge tone="warn">Read only</Badge> : null}
                          {md.exact && c.fail_mode === 'closed' ? (
                            <Badge tone="neutral" title="Requests are refused while the cache is down">
                              Fail closed
                            </Badge>
                          ) : null}
                        </span>
                      </Td>
                      <Td className="w-28">
                        <Switch
                          label={`${md.prompt ? 'Turn off' : 'Turn on'} provider prompt caching for ${m.name}`}
                          hideLabel
                          checked={md.prompt}
                          disabled={!canEdit}
                          disabledReason="You don't have permission to change models."
                          onChange={(next) =>
                            void setMode(
                              m,
                              { prompt_caching: { auto_inject: next } },
                              next ? 'Prompt caching turned on' : 'Prompt caching turned off',
                              { prompt_caching: { auto_inject: !next } },
                            )
                          }
                        />
                      </Td>
                      <Td className="w-[20%]">
                        {rate !== null && u ? (
                          <div>
                            {/* One line, centred in the row: the bar, the rate, and of how many. */}
                            <div className="flex items-center gap-2 text-caption">
                              <Meter
                                value={u.hits}
                                max={u.requests}
                                tone="accent"
                                label={`${m.name} hit rate ${formatPercent(rate, 0)}`}
                                className="min-w-12 flex-1"
                              />
                              <span className="mono w-9 text-right text-fg">{formatPercent(rate, 0)}</span>
                              <span className="mono whitespace-nowrap text-muted">
                                {formatCount(u.hits, { compact: true })} of{' '}
                                {formatCount(u.requests, { compact: true })}
                              </span>
                            </div>
                          </div>
                        ) : (
                          <span className="text-caption text-muted">—</span>
                        )}
                      </Td>
                      <Td numeric className={u && u.saved > 0 ? 'text-success' : undefined}>
                        {u ? formatMoney(u.saved) : '—'}
                      </Td>
                      <Td muted>
                        {md.exact ? `Keep ${duration(c.ttl ?? '1h')} · ${keyStrategyOf(c).label}` : '—'}
                      </Td>
                      <Td className="text-right">
                        <span className="inline-flex items-center gap-1">
                          <Gate operation="patchModel" mode="hide">
                            <Button
                              size="sm"
                              variant="ghost"
                              icon="settings"
                              onClick={() => setConfiguring(m)}
                            >
                              Settings
                            </Button>
                          </Gate>
                          <IconButton
                            icon={open ? 'minus' : 'plus'}
                            label={open ? `Hide details of ${m.name}` : `Show details of ${m.name}`}
                            onClick={() => toggle(m.name)}
                          />
                        </span>
                      </Td>
                    </Tr>
                    {open ? (
                      <tr id={`cache-model-${m.name}`}>
                        <td colSpan={8} className="border-b border-line p-0">
                          <ModelDetail
                            model={m}
                            usage={u}
                            onConfigure={() => setConfiguring(m)}
                            onClear={() => setClearing(m.name)}
                          />
                        </td>
                      </tr>
                    ) : null}
                  </Fragment>
                );
              })}
            </tbody>
          </Table>
        </div>
      ) : null}
      {configuring ? <SettingsDrawer model={configuring} onClose={() => setConfiguring(null)} /> : null}
      {clearing ? (
        <ClearDialog open onClose={() => setClearing(null)} fixed={{ level: 'model', name: clearing }} />
      ) : null}
      <ConfirmDialog
        open={turningOn}
        onClose={() => setTurningOn(false)}
        tone="primary"
        withReason={false}
        title={`Turn on caching for ${plural(offList.length, 'model')}?`}
        confirmLabel="Turn on caching"
        effect="Identical requests to these models are answered from the cache, with each model’s current settings."
        onConfirm={async () => {
          const failed: string[] = [];
          for (const m of offList) {
            try {
              await setModelCache(m.name, { enabled: true });
            } catch {
              failed.push(m.name);
            }
          }
          if (failed.length === 0) notify('success', 'Caching turned on for all models');
          else
            notify(
              'error',
              `Couldn’t turn caching on for ${plural(failed.length, 'model')}`,
              failed.join(', '),
            );
        }}
      >
        <p className="mono text-caption text-muted">{offList.map((m) => m.name).join(', ')}</p>
      </ConfirmDialog>
    </section>
  );
}
