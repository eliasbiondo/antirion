'use client';

import { useEffect, useMemo, useRef, useState } from 'react';

import { Icon } from '@/components/ui/icon';
import { menuKeyHandler, Popover } from '@/components/ui/overlay';
import { EmptyPill } from '@/components/ui/states';
import { api } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import type { OrganizationResource } from '@/lib/api/types';
import { hasPermission, isGatewayPrincipal, useSession } from '@/stores/session';
import { encodeScope, type Scope, scopeOrganization, useScope } from '@/stores/scope';

/** The organization switcher: "All organizations" for gateway principals, or one organization. */
export function ScopeSwitcher({ onCreateOrganization }: { onCreateOrganization?: () => void }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [orgs, setOrgs] = useState<{ name: string; label: string }[] | null>(null);
  const me = useSession((s) => s.me);
  const scope = useScope((s) => s.scope);
  const setScope = useScope((s) => s.setScope);

  useEffect(() => {
    if (!open || orgs) return;
    api
      .get('/organizations', { query: { limit: 500 } })
      .then((r) =>
        setOrgs(
          r.items.map((o: OrganizationResource) => ({
            name: o.name,
            label: o.spec.display_name || o.name,
          })),
        ),
      )
      .catch(() => {
        const names = Object.keys(me?.permissions ?? {})
          .filter((s) => s.startsWith('organization/'))
          .map((s) => s.slice('organization/'.length));
        setOrgs(names.map((name) => ({ name, label: name })));
      });
  }, [open, orgs, me]);

  const options = useMemo(() => {
    const list: { scope: Scope; label: string; detail?: string }[] = [];
    if (isGatewayPrincipal(me))
      list.push({
        scope: { kind: 'gateway' },
        label: 'All organizations',
        detail: 'Everything on this gateway',
      });
    for (const org of orgs ?? []) {
      list.push({
        scope: { kind: 'organization', name: org.name },
        label: org.label,
        ...(org.label !== org.name ? { detail: org.name } : {}),
      });
    }
    const q = query.trim().toLowerCase();
    return q ? list.filter((o) => o.label.toLowerCase().includes(q) || (o.detail ?? '').includes(q)) : list;
  }, [orgs, me, query]);

  const current = scope.kind === 'gateway' ? 'All organizations' : scope.name;
  const canCreate = hasPermission(me, 'organizations:create', scopeOrganization(scope));

  return (
    <div className="relative">
      <button
        ref={anchor}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Scope: ${current}. Change scope`}
        onClick={() => setOpen(!open)}
        className="inline-flex h-7 min-w-0 items-center gap-1.5 rounded-md border border-transparent px-2 text-ui font-semibold text-fg hover:bg-hover aria-expanded:border-line aria-expanded:bg-hover"
      >
        {scope.kind === 'gateway' ? <Icon name="globe" size={14} className="shrink-0 text-muted" /> : null}
        <span className="max-w-[180px] truncate max-sm:max-w-[96px]">{current}</span>
        <Icon name="chevronDown" size={12} className="shrink-0 text-dim" />
      </button>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        label="Change scope"
        className="w-[280px]"
      >
        <div className="flex items-center gap-2 border-b border-line px-2.5 py-2">
          <span className="text-dim">
            <Icon name="search" size={14} />
          </span>
          <input
            autoFocus
            aria-label="Find an organization"
            placeholder="Find an organization"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="flex-1 bg-transparent text-ui text-fg outline-none placeholder:text-dim"
          />
        </div>
        <ul
          role="listbox"
          aria-label="Scopes"
          className="max-h-64 overflow-y-auto p-1"
          onKeyDown={menuKeyHandler}
        >
          {orgs === null && !isGatewayPrincipal(me) ? (
            <li className="px-2 py-3 text-caption text-muted">Loading…</li>
          ) : null}
          {options.length === 0 && orgs !== null ? (
            <li className="flex justify-center px-2 py-3">
              <EmptyPill>No organizations match</EmptyPill>
            </li>
          ) : null}
          {options.map((o) => {
            const on = encodeScope(o.scope) === encodeScope(scope);
            return (
              <li key={encodeScope(o.scope)}>
                <button
                  type="button"
                  role="option"
                  aria-selected={on}
                  onClick={() => {
                    setScope(o.scope);
                    setOpen(false);
                  }}
                  className={cn(
                    'flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left',
                    on ? 'bg-hover' : 'hover:bg-hover',
                  )}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-body font-medium text-fg">{o.label}</span>
                    {o.detail ? (
                      <span className="block truncate text-caption text-dim">{o.detail}</span>
                    ) : null}
                  </span>
                  {on ? (
                    <span className="text-accent">
                      <Icon name="check" size={14} />
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </ul>
        {canCreate && onCreateOrganization ? (
          <div className="border-t border-line bg-canvas p-1">
            <button
              type="button"
              onClick={() => {
                setOpen(false);
                onCreateOrganization();
              }}
              className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-ui text-muted hover:bg-hover hover:text-fg"
            >
              <Icon name="plus" size={14} /> Create organization
            </button>
          </div>
        ) : null}
      </Popover>
    </div>
  );
}

/** Environment scope: "All environments" or a value of the environment tag. */
export function EnvironmentSelect() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const environment = useScope((s) => s.environment);
  const setEnvironment = useScope((s) => s.setEnvironment);
  const scope = useScope((s) => s.scope);
  const [values, setValues] = useState<{ name: string; rpm: number }[] | null>(null);

  useEffect(() => {
    if (!open) return;
    const to = new Date();
    const from = new Date(to.getTime() - 5 * 60_000);
    const org = scopeOrganization(scope);
    api
      .get('/analytics/usage', {
        query: {
          from: from.toISOString(),
          to: to.toISOString(),
          group_by: ['environment'],
          ...(org ? { organization: [org] } : {}),
        },
      })
      .then((r) => {
        const byEnv = new Map<string, number>();
        for (const row of r.items) {
          const name = row.group.environment;
          if (name) byEnv.set(name, (byEnv.get(name) ?? 0) + row.requests / 5);
        }
        setValues([...byEnv.entries()].map(([name, rpm]) => ({ name, rpm })).sort((a, b) => b.rpm - a.rpm));
      })
      .catch(() => setValues([]));
  }, [open, scope]);

  const all = [{ name: '', rpm: values?.reduce((a, v) => a + v.rpm, 0) ?? 0 }, ...(values ?? [])];
  return (
    <div className="relative">
      <button
        ref={anchor}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Environment: ${environment ?? 'all environments'}`}
        onClick={() => setOpen(!open)}
        className="inline-flex h-7 items-center gap-1.5 rounded-control border border-line bg-canvas px-2 text-ui text-fg hover:border-line-strong hover:bg-hover"
      >
        <span
          aria-hidden="true"
          className={cn('h-1.5 w-1.5 rounded-full', environment ? 'bg-success' : 'bg-dim')}
        />
        <span className="whitespace-nowrap">{environment ?? 'All environments'}</span>
        <Icon name="chevronDown" size={12} className="text-dim" />
      </button>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align="right"
        label="Environment"
        className="w-[260px]"
      >
        <div className="border-b border-line px-3 py-2 text-caption font-medium text-muted">Environment</div>
        <ul className="p-1" role="listbox" aria-label="Environments" onKeyDown={menuKeyHandler}>
          {all.map((v) => {
            const on = (environment ?? '') === v.name;
            return (
              <li key={v.name || 'all'}>
                <button
                  type="button"
                  role="option"
                  aria-selected={on}
                  onClick={() => {
                    setEnvironment(v.name || null);
                    setOpen(false);
                  }}
                  className={cn(
                    'flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left',
                    on ? 'bg-hover' : 'hover:bg-hover',
                  )}
                >
                  <span className="flex-1 text-body text-fg">{v.name || 'All environments'}</span>
                  <span className="mono text-caption text-muted tabular-nums">
                    {values ? `${Math.round(v.rpm)}/min` : ''}
                  </span>
                  {on ? (
                    <span className="text-accent">
                      <Icon name="check" size={14} />
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
          {values !== null && values.length === 0 ? (
            <li className="px-2 py-2 text-caption text-muted">
              No requests tagged with an environment in the last 5 minutes.
            </li>
          ) : null}
        </ul>
      </Popover>
    </div>
  );
}
