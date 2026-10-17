'use client';

import { useEffect, useEffectEvent, useMemo, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Checkbox, Input, Segmented, Select } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { FilterBar } from '@/components/ui/layout';
import { ModelName, modelOption } from '@/components/ui/marks';
import { MenuButton } from '@/components/ui/menu';
import { Banner, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import type { CacheEntry } from '@/lib/api/types';
import { formatBytes, formatCount, formatMoney, plural, shortId } from '@/lib/format';
import { type CsvColumn, exportCsv } from '@/lib/csv';
import { formatRelative } from '@/lib/time';
import { useUrlFilters } from '@/lib/url-filters';
import { notify } from '@/stores/ui';

import { cacheApi, type EntryQuery, type EntrySort } from '../api';
import { useCache } from '../store';
import { EntryDrawer, expiresIn } from './entry-drawer';

const FILTERS = { key: '', match: '', model: '', sort: 'last_hit_at:desc' };

const NONE = new Set<string>();

/** The most responses one export pages through. */
const EXPORT_LIMIT = 10_000;

const CSV_COLUMNS: CsvColumn<CacheEntry>[] = [
  { header: 'Key', value: (e) => e.key },
  { header: 'Model', value: (e) => e.model },
  { header: 'Match', value: (e) => e.mode },
  { header: 'Route', value: (e) => e.route ?? '' },
  { header: 'Deployment', value: (e) => e.deployment ?? '' },
  { header: 'Scope', value: (e) => e.scope ?? '' },
  { header: 'Hits', value: (e) => e.hits },
  { header: 'Size (bytes)', value: (e) => e.size_bytes },
  { header: 'Input tokens', value: (e) => e.input_tokens ?? '' },
  { header: 'Output tokens', value: (e) => e.output_tokens ?? '' },
  { header: 'Similarity threshold', value: (e) => e.similarity_threshold ?? '' },
  { header: 'Saved (USD)', value: (e) => e.saved_usd ?? '' },
  { header: 'Cached', value: (e) => e.created_at },
  { header: 'Expires', value: (e) => e.expires_at },
  { header: 'Last hit', value: (e) => e.last_hit_at ?? '' },
];

/** Every response the filters select (up to the limit), paged from the same list, as CSV. */
async function exportEntries(query: EntryQuery): Promise<void> {
  const all: CacheEntry[] = [];
  let cursor: string | undefined;
  do {
    const page = await cacheApi.entries(query, cursor);
    all.push(...page.items);
    cursor = page.next ?? undefined;
  } while (cursor && all.length < EXPORT_LIMIT);
  exportCsv('cached-responses', CSV_COLUMNS, all.slice(0, EXPORT_LIMIT));
}

const SORTS: EntrySort[] = ['hits', 'last_hit_at', 'created_at', 'expires_at', 'size'];

function sortOf(value: string): { sort: EntrySort; order: 'asc' | 'desc' } {
  const [s, o] = value.split(':');
  return {
    sort: SORTS.includes(s as EntrySort) ? (s as EntrySort) : 'last_hit_at',
    order: o === 'asc' ? 'asc' : 'desc',
  };
}

/**
 * The cached responses, newest hit first by default: filters by match, model and key prefix (in the
 * URL), sortable columns, totals of the filtered set, a drawer per response, and bulk removal.
 */
export function EntriesTab() {
  const entries = useCache((s) => s.entries);
  const models = useCache((s) => s.models.data);
  const loadEntries = useCache((s) => s.loadEntries);
  const loadMore = useCache((s) => s.loadMoreEntries);
  const deleteEntries = useCache((s) => s.deleteEntries);
  const deleteEntry = useCache((s) => s.deleteEntry);
  const canDelete = useCan('deleteCacheEntry');
  const canReport = useCan('reportCacheFalsePositive');
  const { filters, set, clear, active } = useUrlFilters(FILTERS);
  const [typed, setTyped] = useState(filters.key);
  // The selection belongs to one listing: a new filter or sort starts with none.
  const [selection, setSelection] = useState<{ of: string; keys: Set<string> }>({ of: '', keys: new Set() });
  // The open response is in the URL (`entry`), so its link can be copied; "Report wrong match" is
  // this visit's only.
  const [entryKey, setEntryKey] = useQueryParam('entry');
  const [report, setReport] = useState(false);
  const [fetched, setFetched] = useState<CacheEntry | null>(null);
  const setOpen = (entry: CacheEntry, reporting = false) => {
    setReport(reporting);
    setEntryKey(entry.key);
  };
  const closeEntry = () => {
    setReport(false);
    setEntryKey(null);
  };
  const [bulk, setBulk] = useState(false);
  const [more, setMore] = useState(false);
  const [exporting, setExporting] = useState(false);

  const { sort, order } = sortOf(filters.sort);
  const hex = /^[0-9a-fA-F]*$/.test(typed.trim());
  const query: EntryQuery = useMemo(
    () => ({
      ...(filters.key ? { q: filters.key } : {}),
      ...(filters.match === 'exact' || filters.match === 'semantic' ? { mode: filters.match } : {}),
      ...(filters.model ? { model: filters.model } : {}),
      sort,
      order,
    }),
    [filters.key, filters.match, filters.model, sort, order],
  );
  const listing = JSON.stringify(query);
  const selected = selection.of === listing ? selection.keys : NONE;
  const setSelected = (keys: Set<string>) => setSelection({ of: listing, keys });

  // The key search waits for a pause in typing, and only searches hexadecimal prefixes.
  useEffect(() => {
    const t = setTimeout(() => {
      const next = typed.trim().toLowerCase();
      if (/^[0-9a-f]*$/.test(next) && next !== filters.key) set({ key: next });
    }, 300);
    return () => clearTimeout(t);
  }, [typed, filters.key, set]);

  useEffect(() => {
    void loadEntries(query);
  }, [loadEntries, query]);

  const page = entries.data;
  const rows = page?.items ?? [];
  const listed = entryKey ? rows.find((e) => e.key === entryKey) : undefined;
  // A link to a response not on the loaded page reads it by its key; one that is gone closes.
  const dropEntry = useEffectEvent(() => setEntryKey(null));
  useEffect(() => {
    if (!entryKey || listed || fetched?.key === entryKey) return;
    let live = true;
    cacheApi.entry(entryKey).then(
      (e) => live && setFetched(e),
      () => live && dropEntry(),
    );
    return () => {
      live = false;
    };
  }, [entryKey, listed, fetched]);
  const opened = listed ?? (fetched?.key === entryKey ? fetched : null);
  const allOn = rows.length > 0 && rows.every((e) => selected.has(e.key));
  const someOn = rows.some((e) => selected.has(e.key));

  const header = (label: string, key: EntrySort, numeric = false) => (
    <Th
      numeric={numeric}
      sort={sort === key ? order : null}
      onSort={() => set({ sort: `${key}:${sort === key && order === 'desc' ? 'asc' : 'desc'}` })}
    >
      {label}
    </Th>
  );
  const toggle = (key: string, on: boolean) => {
    const next = new Set(selected);
    if (on) next.add(key);
    else next.delete(key);
    setSelected(next);
  };

  return (
    <section aria-labelledby="cache-entries-title">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line bg-raised px-gutter pt-4 pb-3 max-sm:px-gutter-sm">
        <h2 id="cache-entries-title" className="text-title font-semibold text-fg">
          Cached responses
        </h2>
        <span className="text-ui text-muted">Open one to see its key and the stored answer.</span>
      </div>
      <FilterBar>
        <Input
          aria-label="Search by key"
          icon="search"
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          placeholder="Search by key"
          className="h-7 w-52"
          aria-invalid={!hex}
        />
        <Segmented<string>
          label="Match"
          value={filters.match === 'exact' || filters.match === 'semantic' ? filters.match : ''}
          onChange={(match) => set({ match })}
          options={[
            { value: '', label: 'All' },
            { value: 'exact', label: 'Exact' },
            { value: 'semantic', label: 'Similar' },
          ]}
        />
        <div className="w-52">
          <Select
            aria-label="Model"
            size="sm"
            value={filters.model}
            onChange={(model) => set({ model })}
            emptyOption="Every model"
            searchable
            options={(models ?? []).map((m) => modelOption(m))}
          />
        </div>
        {!hex ? <span className="text-caption text-error">Keys are hexadecimal: 0–9 and a–f.</span> : null}
        {rows.length > 0 ? (
          <Button
            icon="download"
            variant="ghost"
            className="ml-auto"
            busy={exporting}
            onClick={() => {
              setExporting(true);
              exportEntries(query)
                .catch(() => notify('error', "Couldn't export the cached responses"))
                .finally(() => setExporting(false));
            }}
          >
            Export responses (CSV)
          </Button>
        ) : null}
      </FilterBar>
      {page && !page.enumerable ? (
        <Banner tone="warn" title="The cache can’t list its responses right now">
          Its index couldn’t be read, usually because the storage is down. Cached answers may still be served.
        </Banner>
      ) : null}
      {someOn ? (
        <div className="flex flex-wrap items-center gap-3 border-b border-line bg-accent-dim px-gutter py-2 max-sm:px-gutter-sm">
          <span className="text-ui font-semibold text-fg">{plural(selected.size, 'response')} selected</span>
          <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
            Clear selection
          </Button>
          {canDelete ? (
            <Button size="sm" variant="danger" icon="trash" className="ml-auto" onClick={() => setBulk(true)}>
              Remove selected
            </Button>
          ) : null}
        </div>
      ) : null}
      {entries.status === 'loading' && !page ? <LoadingRows rows={4} /> : null}
      {entries.error ? (
        <ErrorState
          error={entries.error}
          subject="Cached responses"
          onRetry={() => void loadEntries(query)}
        />
      ) : null}
      {page && page.enumerable && rows.length === 0 ? (
        active ? (
          <NoMatch
            onClear={() => {
              setTyped('');
              clear();
            }}
          />
        ) : (
          <EmptyState
            title="Nothing cached yet"
            description="Answers show here once a model with caching on responds."
          />
        )
      ) : null}
      {rows.length > 0 ? (
        <div className="overflow-x-auto border-b border-line bg-raised">
          <Table label="Cached responses">
            <thead>
              <tr>
                <Th className="w-10">
                  <Checkbox
                    aria-label="Select every response shown"
                    checked={allOn ? true : someOn ? 'mixed' : false}
                    onChange={(on) => setSelected(on ? new Set(rows.map((e) => e.key)) : new Set())}
                  />
                </Th>
                <Th>Key</Th>
                <Th>Model</Th>
                <Th>Match</Th>
                {header('Hits', 'hits', true)}
                {header('Size', 'size', true)}
                <Th numeric title="The similarity a prompt needed to match this response">
                  Similarity
                </Th>
                <Th numeric>Saved</Th>
                {header('Cached', 'created_at')}
                {header('Expires', 'expires_at')}
                {header('Last hit', 'last_hit_at')}
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {rows.map((e) => (
                <Tr key={e.key} selected={selected.has(e.key)}>
                  <Td>
                    <Checkbox
                      aria-label={`Select ${shortId(e.key, 12)}`}
                      checked={selected.has(e.key)}
                      onChange={(on) => toggle(e.key, on)}
                    />
                  </Td>
                  <Td mono title={e.key}>
                    <button type="button" className="text-accent hover:underline" onClick={() => setOpen(e)}>
                      {shortId(e.key, 16)}
                    </button>
                  </Td>
                  <Td>
                    <ModelName model={e.model} className="text-fg" />
                  </Td>
                  <Td>
                    <Badge tone={e.mode === 'semantic' ? 'info' : 'neutral'}>
                      {e.mode === 'semantic' ? 'Similar' : 'Exact'}
                    </Badge>
                  </Td>
                  <Td numeric>{formatCount(e.hits)}</Td>
                  <Td numeric>{formatBytes(e.size_bytes)}</Td>
                  <Td numeric muted>
                    {e.similarity_threshold != null ? e.similarity_threshold.toFixed(2) : '—'}
                  </Td>
                  <Td numeric className={Number(e.saved_usd ?? 0) > 0 ? 'text-success' : undefined}>
                    {e.saved_usd !== undefined ? formatMoney(e.saved_usd) : '—'}
                  </Td>
                  <Td muted>{formatRelative(e.created_at)}</Td>
                  <Td muted>{expiresIn(e.expires_at)}</Td>
                  <Td muted>{e.last_hit_at ? formatRelative(e.last_hit_at) : 'Never'}</Td>
                  <Td className="text-right">
                    <MenuButton
                      iconOnly
                      size="sm"
                      variant="ghost"
                      label={`Actions for ${shortId(e.key, 12)}`}
                      items={[
                        { label: 'Open', icon: 'eye', onSelect: () => setOpen(e) },
                        ...(e.mode === 'semantic' && canReport
                          ? [
                              {
                                label: 'Report wrong match',
                                icon: 'alert' as const,
                                onSelect: () => setOpen(e, true),
                              },
                            ]
                          : []),
                        ...(canDelete
                          ? [
                              {
                                label: 'Remove',
                                icon: 'trash' as const,
                                danger: true,
                                onSelect: () =>
                                  void deleteEntry(e.key).then(
                                    () => notify('success', 'Response removed', e.model),
                                    () => undefined,
                                  ),
                              },
                            ]
                          : []),
                      ]}
                    />
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-gutter py-2.5 text-caption text-muted max-sm:px-gutter-sm">
            <span>
              Showing {formatCount(rows.length)}
              {page?.total != null ? ` of ${formatCount(page.total)}` : ''}
              {active ? ' matching' : ''}
            </span>
            {page?.totalBytes != null ? <span>{formatBytes(page.totalBytes)} in all</span> : null}
            {page?.totalHits != null ? <span>{plural(page.totalHits, 'hit')} in all</span> : null}
            {page?.next ? (
              <Button
                size="sm"
                variant="secondary"
                className="ml-auto"
                busy={more}
                onClick={() => {
                  setMore(true);
                  void loadMore(query).finally(() => setMore(false));
                }}
              >
                Show more
              </Button>
            ) : null}
          </div>
        </div>
      ) : null}
      {opened ? (
        <EntryDrawer
          key={opened.key}
          entry={opened}
          startWith={report ? 'report' : undefined}
          onClose={closeEntry}
        />
      ) : null}
      <ConfirmDialog
        open={bulk}
        onClose={() => setBulk(false)}
        title={`Remove ${plural(selected.size, 'cached response')}?`}
        confirmLabel="Remove responses"
        effect="The next identical requests go to the provider and are cached again."
        onConfirm={async (reason) => {
          const result = await deleteEntries([...selected], reason);
          setSelected(new Set(result.failed.map((f) => f.key)));
          if (result.failed.length === 0) notify('success', `${plural(result.removed, 'response')} removed`);
          else
            notify(
              'error',
              `${plural(result.failed.length, 'response')} couldn’t be removed`,
              `${plural(result.removed, 'response')} removed; the others are still selected.`,
            );
        }}
      />
    </section>
  );
}
