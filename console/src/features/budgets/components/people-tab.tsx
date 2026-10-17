'use client';

import Link from 'next/link';
import { useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { IconButton } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { FilterBar } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { CountNote, ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { formatCount, formatMoney } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { budgetsApi } from '../api';
import { usePeople } from '../hooks';
import { peopleNote, type Person } from '../people';
import { useBudgets } from '../store';
import type { BudgetRow } from '../types';

/** A note to a person about their spending, by the gateway's notification channels. */
export function NotifyDialog({
  person,
  row,
  onClose,
}: {
  person: Person | null;
  row: BudgetRow | null;
  onClose: () => void;
}) {
  const [subject, setSubject] = useState('');
  const [message, setMessage] = useState('');
  const shown = person !== null;
  const defaultSubject = 'Your AI spending this month';
  const defaultMessage = person
    ? row
      ? `You've spent ${formatMoney(row.spent)} of ${formatMoney(row.amount)} in ${row.owner} · ${row.name} this period. Take a look at what is using it.`
      : `You've spent ${formatMoney(person.cost)} on ${formatCount(person.requests)} requests this month. Take a look at what is using it.`
    : '';
  return (
    <ConfirmDialog
      open={shown}
      onClose={() => {
        setSubject('');
        setMessage('');
        onClose();
      }}
      title={`Notify ${person?.displayName ?? ''}?`}
      tone="primary"
      confirmLabel="Send note"
      withReason={false}
      effect="They get this note by email and in their notification channels, with a link to Budgets."
      onConfirm={async () => {
        if (!person) return;
        const link = typeof window !== 'undefined' ? window.location.href : undefined;
        await budgetsApi.notify(
          person.name,
          subject.trim() || defaultSubject,
          message.trim() || defaultMessage,
          link,
        );
        notify('success', 'Note sent', person.displayName);
      }}
    >
      <Field label="Subject">
        <Input value={subject} onChange={(e) => setSubject(e.target.value)} placeholder={defaultSubject} />
      </Field>
      <Field label="Message">
        <Textarea
          rows={3}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder={defaultMessage}
        />
      </Field>
    </ConfirmDialog>
  );
}

type Sort = 'cost' | 'requests' | 'per' | 'active';

function PersonRow({
  p,
  teamName,
  onNotify,
  canNotify,
}: {
  p: Person;
  teamName: string | null;
  onNotify: () => void;
  canNotify: boolean;
}) {
  const href = useScopedHref('/teams/', { tab: 'people', user: p.name });
  return (
    <Tr>
      <Td className="whitespace-normal">
        <div className="flex flex-wrap items-center gap-2">
          <Link href={href} className="text-body font-semibold text-fg hover:underline">
            {p.displayName}
          </Link>
          {p.service ? <Badge title="A service account">Service account</Badge> : null}
          {p.outlier ? (
            <Badge tone="warn" title="Spending more than twice the mean of their team">
              Outlier
            </Badge>
          ) : null}
        </div>
        <div className="text-caption text-muted">
          <span className="mono">{p.name}</span>
          {p.title ? ` · ${p.title}` : ''}
        </div>
      </Td>
      <Td muted mono>
        {teamName ?? '—'}
      </Td>
      <Td numeric>{formatCount(p.requests)}</Td>
      <Td numeric>{formatMoney(p.cost)}</Td>
      <Td numeric>{p.perRequest !== null ? formatMoney(p.perRequest) : '—'}</Td>
      <Td muted>{p.lastActive ? formatRelative(p.lastActive) : 'Never'}</Td>
      <Td className="text-right whitespace-nowrap">
        {canNotify ? <IconButton icon="bell" label={`Notify ${p.displayName}`} onClick={onNotify} /> : null}
        <Tooltip content={`Open ${p.displayName}'s profile`}>
          <Link
            href={href}
            aria-label={`Open ${p.displayName}'s profile`}
            className="inline-flex h-7 w-7 items-center justify-center rounded-control text-muted hover:bg-hover hover:text-fg"
          >
            <Icon name="user" size={14} />
          </Link>
        </Tooltip>
      </Td>
    </Tr>
  );
}

/** People and what they spent this month: requests, spend, cost per request, marks and actions. */
export function PeopleTab() {
  const people = usePeople();
  const error = useBudgets((s) => s.users.error ?? s.people.error);
  const canNotify = useCan('sendNotification');
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<Sort>('cost');
  const [notifying, setNotifying] = useState<Person | null>(null);
  const q = query.trim().toLowerCase();
  const rows = (people ?? [])
    .filter(
      (p) => !q || `${p.name} ${p.displayName} ${p.title ?? ''} ${p.team ?? ''}`.toLowerCase().includes(q),
    )
    .sort((a, b) =>
      sort === 'requests'
        ? b.requests - a.requests
        : sort === 'per'
          ? (b.perRequest ?? -1) - (a.perRequest ?? -1)
          : sort === 'active'
            ? (b.lastActive ?? '').localeCompare(a.lastActive ?? '')
            : b.cost - a.cost,
    );
  if (error) return <ErrorState error={error} subject="People" />;
  if (!people) return <LoadingRows />;
  if (people.length === 0)
    return <EmptyState title="No people yet" description="People who own keys show here with their spend." />;
  return (
    <>
      <FilterBar>
        <div className="w-64 max-sm:w-full">
          <Input
            aria-label="Search people"
            icon="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search people"
          />
        </div>
        <span className="ml-auto flex items-center gap-3">
          <CountNote shown={rows.length} total={people.length} noun="people" singular="person" />
          <ExportCsvButton
            filename="budget-people.csv"
            rows={rows}
            columns={[
              { header: 'user', value: (p) => p.name },
              { header: 'name', value: (p) => p.displayName },
              { header: 'title', value: (p) => p.title },
              { header: 'team', value: (p) => p.team },
              { header: 'service_account', value: (p) => p.service },
              { header: 'requests', value: (p) => p.requests },
              { header: 'spend_usd', value: (p) => p.cost.toFixed(4) },
              { header: 'usd_per_request', value: (p) => (p.perRequest ?? 0).toFixed(6) },
              { header: 'outlier', value: (p) => p.outlier },
              { header: 'last_active_at', value: (p) => p.lastActive },
            ]}
          />
        </span>
      </FilterBar>
      <Table label="People and their spend this month" className="border-b border-line bg-raised">
        <thead>
          <tr>
            <Th>Person</Th>
            <Th>Team</Th>
            <Th numeric sort={sort === 'requests' ? 'desc' : null} onSort={() => setSort('requests')}>
              Requests
            </Th>
            <Th numeric sort={sort === 'cost' ? 'desc' : null} onSort={() => setSort('cost')}>
              Spend this month
            </Th>
            <Th numeric sort={sort === 'per' ? 'desc' : null} onSort={() => setSort('per')}>
              Per request
            </Th>
            <Th sort={sort === 'active' ? 'desc' : null} onSort={() => setSort('active')}>
              Last active
            </Th>
            <Th>
              <span className="sr-only">Actions</span>
            </Th>
          </tr>
        </thead>
        <tbody>
          {rows.map((p) => (
            <PersonRow
              key={p.name}
              p={p}
              teamName={p.team}
              canNotify={canNotify}
              onNotify={() => setNotifying(p)}
            />
          ))}
        </tbody>
      </Table>
      <p className="px-5 py-3 text-caption text-muted max-sm:px-gutter-sm">{peopleNote(people)}</p>
      <NotifyDialog person={notifying} row={null} onClose={() => setNotifying(null)} />
    </>
  );
}
