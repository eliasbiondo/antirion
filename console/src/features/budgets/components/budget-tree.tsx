'use client';

import Link from 'next/link';
import { Fragment, useMemo } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { IconButton } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Meter } from '@/components/ui/charts';
import { useCan } from '@/components/ui/gate';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Td, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, formatPercent, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';

import type { Spend } from '../api';
import { modelsCovered } from '../form';
import { useDeclarations, usePeople } from '../hooks';
import type { Person } from '../people';
import { useBudgets } from '../store';
import {
  type BudgetRow,
  enforcementLabel,
  flatten,
  isCounted,
  LEVEL_LABEL,
  partitionLabel,
  partitionNoun,
  STATE_LABEL,
  stateTone,
} from '../types';

export type StateFilter = 'all' | 'over' | 'at_risk' | 'on_pace';

export function matches(row: BudgetRow, filter: StateFilter): boolean {
  if (filter === 'all') return true;
  if (filter === 'over') return row.state === 'over' || row.state === 'blocked';
  return row.state === filter;
}

/** What a row's actions open: the drawer, a dialog to approve going over, or a note to a person. */
export interface RowActions {
  onEdit: (row: BudgetRow) => void;
  onApprove: (row: BudgetRow) => void;
  onNotify: (person: Person, row: BudgetRow) => void;
}

/** The figures rows show beside their counter: Models, projects, people and their month. */
export interface TreeFacts {
  declaration: ReturnType<typeof useDeclarations>;
  modelsUsed: Map<string, Map<string, Spend>> | null;
  projectsOfTeam: Map<string, string[]>;
  peopleOfTeam: Map<string, number>;
  people: Map<string, Person>;
  rows: Map<string, BudgetRow>;
}

export function useTreeFacts(tree: BudgetRow[]): TreeFacts {
  const declaration = useDeclarations();
  const modelsUsed = useBudgets((s) => s.modelsUsed.data);
  const projects = useBudgets((s) => s.projects.data);
  const people = usePeople();
  return useMemo(() => {
    const projectsOfTeam = new Map<string, string[]>();
    for (const p of projects ?? [])
      projectsOfTeam.set(p.spec.team, [...(projectsOfTeam.get(p.spec.team) ?? []), p.name]);
    const peopleOfTeam = new Map<string, number>();
    for (const p of people ?? []) if (p.team) peopleOfTeam.set(p.team, (peopleOfTeam.get(p.team) ?? 0) + 1);
    return {
      declaration,
      modelsUsed,
      projectsOfTeam,
      peopleOfTeam,
      people: new Map((people ?? []).map((p) => [p.name, p])),
      rows: new Map(flatten(tree).map((r) => [r.id, r])),
    };
  }, [declaration, modelsUsed, projects, people, tree]);
}

function ForecastBar({ row }: { row: BudgetRow }) {
  const over = row.amount > 0 ? row.projected / row.amount - 1 : 0;
  const tone = stateTone(row.state);
  if (row.rolling)
    return (
      <div className="flex flex-col gap-1">
        <Meter
          value={row.spent}
          max={row.amount}
          tone={tone === 'success' ? 'accent' : tone}
          label={`${row.owner} ${row.name}: ${formatMoney(row.spent)} of ${formatMoney(row.amount)} in the window`}
          className="h-2"
        />
        <span className="text-caption text-dim">Rolling window: no period end to forecast</span>
      </div>
    );
  return (
    <div className="flex items-center gap-2">
      <Meter
        value={row.spent}
        max={row.amount}
        projected={row.projected}
        tick={row.kind === 'budget' || row.kind === 'partition' ? row.elapsed : undefined}
        tone={tone === 'success' ? 'accent' : tone}
        label={`${row.owner} ${row.name}: ${formatMoney(row.spent)} spent, projected ${formatMoney(row.projected)} of ${formatMoney(row.amount)}`}
        className="h-2"
      />
      {over > 0 ? (
        <span className={cn('mono shrink-0 text-caption', tone === 'error' ? 'text-error' : 'text-warn')}>
          +{formatPercent(over, 0)}
        </span>
      ) : null}
    </div>
  );
}

function periodText(row: BudgetRow, decl: Record<string, unknown> | undefined): string | null {
  const period = typeof decl?.period === 'string' ? decl.period : null;
  if (row.rolling && period)
    return `rolling ${period.replace(/(\d+)d$/, '$1 days').replace(/(\d+)h$/, '$1 h')}`;
  return null;
}

/** The first line's badges: counts of what is inside, and a person's marks. */
function RowBadges({ row, facts, person }: { row: BudgetRow; facts: TreeFacts; person: Person | undefined }) {
  const inner = row.children.filter((c) => c.kind === 'budget' || c.kind === 'partitioned');
  const overInside = flatten(row.children).filter(
    (c) => isCounted(c) && (c.state === 'over' || c.state === 'blocked'),
  ).length;
  const riskInside = flatten(row.children).filter((c) => isCounted(c) && c.state === 'at_risk').length;
  const out: React.ReactNode[] = [];
  if (row.kind === 'partitioned' || row.kind === 'unassigned') {
    const attr = row.children.find((c) => c.partition)?.partition?.attr ?? 'user';
    out.push(<Badge key="n">{`${row.partitions} ${partitionNoun(attr, row.partitions)}`}</Badge>);
  }
  if (row.kind === 'budget' && row.level === 'team') {
    const projects = facts.projectsOfTeam.get(row.owner)?.length ?? 0;
    const people = facts.peopleOfTeam.get(row.owner) ?? 0;
    if (projects > 0) out.push(<Badge key="p">{plural(projects, 'project')}</Badge>);
    if (people > 0) out.push(<Badge key="u">{`${people} ${people === 1 ? 'person' : 'people'}`}</Badge>);
  } else if (inner.length > 0 && row.kind === 'budget') {
    out.push(<Badge key="i">{plural(inner.length, 'budget')} inside</Badge>);
  }
  if (overInside > 0) out.push(<Badge key="o" tone="error">{`${overInside} over`}</Badge>);
  else if (riskInside > 0) out.push(<Badge key="r" tone="warn">{`${riskInside} at risk`}</Badge>);
  if (person?.service)
    out.push(
      <Badge key="s" title="A service account">
        Service account
      </Badge>,
    );
  if (person?.outlier)
    out.push(
      <Badge key="x" tone="warn" title="Spending more than twice the mean of their team">
        Outlier
      </Badge>,
    );
  if (row.kind === 'partition' && row.partition?.value) {
    const decl = facts.declaration(row.level, row.owner, row.name);
    const overrides = (decl?.overrides ?? {}) as Record<string, unknown>;
    if (row.partition.value in overrides)
      out.push(
        <Badge key="a" tone="info">
          Own amount
        </Badge>,
      );
  }
  return <>{out}</>;
}

function RowFacts({
  row,
  facts,
  parent,
}: {
  row: BudgetRow;
  facts: TreeFacts;
  parent: BudgetRow | undefined;
}) {
  const decl = facts.declaration(row.level, row.owner, row.name);
  const used = facts.modelsUsed?.get(`${row.level}/${row.owner}`);
  const covered = modelsCovered(decl);
  const parts: React.ReactNode[] = [];
  if (row.kind === 'budget' || row.kind === 'partitioned') {
    const usedList = used
      ? [...used.entries()].filter(([, s]) => s.requests > 0).sort((a, b) => b[1].cost - a[1].cost)
      : [];
    parts.push(
      <Tooltip
        key="m"
        content={usedList.length > 0 ? `Used this month: ${usedList.map(([m]) => m).join(', ')}` : null}
      >
        <span>
          {covered.label}
          {used ? ` · ${plural(usedList.length, 'Model')} used` : ''}
        </span>
      </Tooltip>,
    );
  }
  if (
    parent &&
    (parent.kind === 'budget' || parent.kind === 'partitioned') &&
    row.kind !== 'partition' &&
    row.kind !== 'unassigned'
  ) {
    const level = (LEVEL_LABEL[parent.level] ?? parent.level).toLowerCase();
    parts.push(
      <Tooltip
        key="i"
        content="Every budget that applies is checked; one inside another doesn't use the outer one's amount."
      >
        <span>Independent of the {level} budget</span>
      </Tooltip>,
    );
    const total = (owner: string, level: string) =>
      [...(facts.modelsUsed?.get(`${level}/${owner}`)?.values() ?? [])].reduce((a, s) => a + s.cost, 0);
    const teamSpend = row.level === 'project' && parent.level === 'team' ? total(parent.owner, 'team') : 0;
    if (teamSpend > 0)
      parts.push(
        <span key="s">
          {formatPercent(total(row.owner, 'project') / teamSpend, 0)} of the team&apos;s spend this month
        </span>,
      );
  }
  if (parts.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-x-1.5 text-caption text-dim">
      {parts.map((p, i) => (
        <Fragment key={i}>
          {i > 0 ? <span aria-hidden="true">·</span> : null}
          {p}
        </Fragment>
      ))}
    </div>
  );
}

function PersonLine({ person }: { person: Person }) {
  return (
    <div className="text-caption text-dim">
      {person.title ? `${person.title} · ` : ''}
      {formatCount(person.requests)} requests this month
      {person.perRequest !== null ? ` · ${formatMoney(person.perRequest)} per request` : ''}
      {person.lastActive ? ` · active ${formatRelative(person.lastActive)}` : ''}
    </div>
  );
}

function Row({
  row,
  depth,
  expanded,
  toggle,
  facts,
  actions,
}: {
  row: BudgetRow;
  depth: number;
  expanded: boolean;
  toggle: (id: string) => void;
  facts: TreeFacts;
  actions: RowActions;
}) {
  const canGrant = useCan('grantBudgetOverage');
  const canNotify = useCan('sendNotification');
  const user = row.partition?.attr === 'user' && row.partition.value ? row.partition.value : null;
  const person = user ? facts.people.get(user) : undefined;
  const profileHref = useScopedHref('/teams/', user ? { tab: 'people', user } : undefined);
  const parent = row.parentId ? facts.rows.get(row.parentId) : undefined;
  const decl = facts.declaration(row.level, row.owner, row.name);
  const group = row.kind === 'partitioned' || row.kind === 'unassigned';
  const title =
    row.kind === 'unassigned'
      ? 'Unassigned'
      : row.kind === 'partition' && row.partition
        ? (person?.displayName ?? partitionLabel(row.partition))
        : row.owner;
  const caption =
    row.kind === 'unassigned'
      ? `People whose team has no budget · their own amounts in ${row.owner} · ${row.name}`
      : row.kind === 'partition'
        ? `${parent && parent.level !== row.level ? `Seat in ${row.owner} · ` : ''}${row.name} · ${enforcementLabel(row.counter)}`
        : null;
  const extra: MenuItem[] = [];
  if (isCounted(row) && canGrant && (row.state === 'blocked' || row.counter.mode === 'approval'))
    extra.push({ label: 'Approve overage', icon: 'check', onSelect: () => actions.onApprove(row) });
  if (person && canNotify)
    extra.push({
      label: `Notify ${person.displayName}`,
      icon: 'bell',
      onSelect: () => actions.onNotify(person, row),
    });
  return (
    <Tr className={cn(row.kind === 'unassigned' && 'bg-sunken/40')}>
      <Td className="whitespace-normal">
        <div className="flex items-center gap-2">
          <span
            aria-hidden="true"
            className={cn(
              'inline-block shrink-0',
              ['w-0', 'w-5', 'w-10', 'w-14', 'w-16'][Math.min(depth, 4)],
            )}
          />
          <span className="inline-flex w-5 shrink-0 justify-center">
            {row.children.length > 0 ? (
              <IconButton
                icon={expanded ? 'chevronDown' : 'chevronRight'}
                label={expanded ? `Collapse ${title}` : `Expand ${title}`}
                onClick={() => toggle(row.id)}
                className="h-5 w-5"
              />
            ) : null}
          </span>
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
              <span className="text-body font-semibold text-fg">{title}</span>
              {row.kind === 'partition' && user ? null : (
                <span className="text-caption text-dim">
                  {row.kind === 'partition' && row.partition
                    ? partitionNoun(row.partition.attr, 1)
                    : row.kind === 'unassigned'
                      ? ''
                      : (LEVEL_LABEL[row.level] ?? row.level)}
                </span>
              )}
              <RowBadges row={row} facts={facts} person={person} />
            </div>
            <div className="text-caption text-muted">
              {caption ?? (
                <>
                  <span className="mono">{row.name}</span>
                  {group ? ` · each with its own amount` : ''} · {enforcementLabel(row.counter)}
                  {periodText(row, decl) ? ` · ${periodText(row, decl)}` : ''}
                  {row.budgetOwner ? ` · owner ${row.budgetOwner}` : ''}
                </>
              )}
            </div>
            {person ? <PersonLine person={person} /> : <RowFacts row={row} facts={facts} parent={parent} />}
          </div>
        </div>
      </Td>
      <Td>
        <Badge tone={stateTone(row.state)} dot>
          {STATE_LABEL[row.state]}
        </Badge>
      </Td>
      <Td numeric>
        <div>{formatMoney(row.spent, { compact: true })}</div>
        <div className="text-caption text-muted">
          {row.amount > 0
            ? `${formatPercent(row.spent / row.amount, 0)} of ${formatMoney(row.amount, { compact: true })}${group ? ' together' : ''}`
            : 'No amount'}
        </div>
      </Td>
      <Td className="w-[28%] min-w-[200px]">
        <ForecastBar row={row} />
      </Td>
      <Td numeric>
        {row.rolling ? (
          <>
            <div className="text-dim">—</div>
            <div className="text-caption text-muted">No forecast</div>
          </>
        ) : (
          <>
            <div className={cn(row.projected > row.amount && 'text-error')}>
              {formatMoney(row.projected, { compact: true })}
            </div>
            <div className="text-caption text-muted">
              {row.projected > row.amount
                ? `${formatMoney(row.projected - row.amount, { compact: true, sign: true })} over`
                : `${formatMoney(row.amount - row.projected, { compact: true })} to spare`}
            </div>
          </>
        )}
      </Td>
      <Td className="text-right whitespace-nowrap">
        <span className="inline-flex items-center justify-end gap-0.5">
          {person ? (
            <Tooltip content={`Open ${person.displayName}'s profile`}>
              <Link
                href={profileHref}
                aria-label={`Open ${person.displayName}'s profile`}
                className="inline-flex h-7 w-7 items-center justify-center rounded-control text-muted hover:bg-hover hover:text-fg"
              >
                <Icon name="user" size={14} />
              </Link>
            </Tooltip>
          ) : null}
          {extra.length > 0 ? (
            <MenuButton
              items={extra}
              label={`More for ${title}`}
              icon="more"
              iconOnly
              variant="ghost"
              size="sm"
            />
          ) : null}
          {row.kind !== 'unassigned' ? (
            <IconButton icon="edit" label={`Edit ${title} ${row.name}`} onClick={() => actions.onEdit(row)} />
          ) : null}
        </span>
      </Td>
    </Tr>
  );
}

/** The budget rows, nested; a row opens by itself when something inside it is over or at risk. */
export function BudgetRows({
  rows,
  depth,
  filter,
  open,
  toggle,
  facts,
  actions,
}: {
  rows: BudgetRow[];
  depth: number;
  filter: StateFilter;
  open: Set<string>;
  toggle: (id: string) => void;
  facts: TreeFacts;
  actions: RowActions;
}) {
  return (
    <>
      {rows.map((row) => {
        const visibleChildren = row.children.filter(
          (c) =>
            (isCounted(c) && matches(c, filter)) ||
            flatten(c.children).some((g) => isCounted(g) && matches(g, filter)),
        );
        const show = (isCounted(row) && matches(row, filter)) || visibleChildren.length > 0;
        if (!show) return null;
        const auto = flatten(row.children).some((c) => isCounted(c) && c.state !== 'on_pace');
        const expanded = open.has(row.id) !== auto;
        return (
          <Fragment key={row.id}>
            <Row
              row={row}
              depth={depth}
              expanded={expanded}
              toggle={toggle}
              facts={facts}
              actions={actions}
            />
            {expanded ? (
              <BudgetRows
                rows={filter === 'all' ? row.children : visibleChildren}
                depth={depth + 1}
                filter={filter}
                open={open}
                toggle={toggle}
                facts={facts}
                actions={actions}
              />
            ) : null}
          </Fragment>
        );
      })}
    </>
  );
}
