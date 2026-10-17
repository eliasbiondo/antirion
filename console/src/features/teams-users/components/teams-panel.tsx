'use client';

import { useEffect, useMemo, useState } from 'react';

import {
  FavouriteButton,
  StarredFilter,
  useFavouriteNames,
  useStarredFilter,
} from '@/components/ui/favourite';
import { Panel, TileGrid } from '@/components/ui/layout';
import { Badge } from '@/components/ui/badge';
import { useQueryParam } from '@/components/layout/url-sync';
import { Button, IconButton } from '@/components/ui/button';
import { BarRow, Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate } from '@/components/ui/gate';
import { Avatar, ColorChip } from '@/components/ui/misc';
import { ChipToggle } from '@/components/ui/form';
import { EmptyAction, EmptyState } from '@/components/ui/states';
import { ExportCsvButton, Truncate } from '@/components/ui/table';
import type { BudgetCounter } from '@/lib/api/types';
import type { CsvColumn } from '@/lib/csv';
import { formatMoney, formatPercent, plural } from '@/lib/format';
import { formatDate } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { useDirectory, useTeams } from '../store';
import type { TeamView } from '../types';
import { TeamFormDrawer } from './forms';

function useTeamSpend(): Map<string, BudgetCounter> {
  const rows = useDirectory((s) => s.teamSpend.data);
  const load = useDirectory((s) => s.loadTeamSpend);
  useEffect(() => {
    void load();
  }, [load]);
  return useMemo(
    () => new Map((rows ?? []).filter((r) => r.unit === 'usd').map((r) => [r.owner, r])),
    [rows],
  );
}

/** Users per team, largest first; a bar lists the team's people. */
function UsersPerTeam({ teams, onTeam }: { teams: TeamView[]; onTeam: (team: string) => void }) {
  const rows = [...teams]
    .filter((t) => t.members.length > 0)
    .sort((a, b) => b.members.length - a.members.length);
  if (rows.length === 0) return null;
  const max = rows[0]?.members.length ?? 1;
  return (
    <Panel
      title="Users per team"
      subtitle="People whose team it is or who hold a role on it"
      className="border-b border-line"
    >
      <ul aria-label="Users per team" className="grid grid-cols-1 gap-x-8 gap-y-1 lg:grid-cols-2">
        {rows.map((t) => (
          <BarRow
            key={t.name}
            name={
              <>
                <ColorChip color={t.color} className="h-2.5 w-2.5" />
                <span className="truncate">{t.displayName}</span>
              </>
            }
            value={t.members.length}
            max={max}
            color={t.color}
            display={<span className="inline-block min-w-16">{plural(t.members.length, 'user')}</span>}
            label={`${t.displayName}: ${plural(t.members.length, 'user')}`}
            onSelect={() => onTeam(t.name)}
          />
        ))}
      </ul>
    </Panel>
  );
}

/** The team's columns of the teams CSV, with this month's spend and budget. */
function teamColumns(spend: Map<string, BudgetCounter>): CsvColumn<TeamView>[] {
  return [
    { header: 'Team', value: (t) => t.name },
    { header: 'Display name', value: (t) => t.displayName },
    { header: 'Organization', value: (t) => t.organization },
    { header: 'Lead', value: (t) => t.lead ?? '' },
    { header: 'Members', value: (t) => t.members.length },
    { header: 'Projects', value: (t) => t.projects.join(' ') },
    { header: 'Keys', value: (t) => t.keys },
    { header: 'Spent this month (USD)', value: (t) => spend.get(t.name)?.spent ?? '' },
    {
      header: 'Monthly budget (USD)',
      value: (t) => spend.get(t.name)?.amount ?? t.monthlyBudget ?? '',
    },
    { header: 'Enabled', value: (t) => t.team.spec.enabled !== false },
    { header: 'Created', value: (t) => t.team.metadata.created_at },
  ];
}

/** Copies the link that opens this tab on one team. */
function TeamLinkButton({ name, label }: { name: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <IconButton
      icon={done ? 'check' : 'link'}
      label={done ? 'Copied' : `Copy link to team ${label}`}
      className="-mt-1"
      onClick={() => {
        const url = new URL(window.location.href);
        url.searchParams.set('tab', 'teams');
        url.searchParams.set('team', name);
        void navigator.clipboard.writeText(url.toString()).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        });
      }}
    />
  );
}

/** The teams grid: colour, lead, members, keys, month-to-date spend against the budget. */
export function TeamsPanel({
  teams: all,
  onNew,
  onTeam,
}: {
  teams: TeamView[];
  onNew: () => void;
  /** Opens the users list filtered to a team. */
  onTeam: (team: string) => void;
}) {
  const starred = useStarredFilter('team');
  const starredNames = useFavouriteNames('team');
  // A team's link (`team=<name>` on this tab) shows that team alone.
  const [focusParam, setFocus] = useQueryParam('team');
  const focus = all.find((t) => t.name === focusParam) ?? null;
  const shown = focus ? [focus] : all;
  const teams = [...starred.apply(shown, (t) => t.name)].sort(
    (a, b) => Number(starredNames.has(b.name)) - Number(starredNames.has(a.name)),
  );
  const timeZone = usePreferences((s) => s.timeZone);
  const remove = useTeams((s) => s.remove);
  const spend = useTeamSpend();
  const [editing, setEditing] = useState<TeamView | null>(null);
  const [deleting, setDeleting] = useState<TeamView | null>(null);

  if (all.length === 0) {
    return (
      <EmptyState
        title="No teams yet"
        description="Teams group people and projects, with their own budget."
        action={
          <Gate operation="createTeam" mode="hide">
            <EmptyAction onClick={onNew}>Create team</EmptyAction>
          </Gate>
        }
      />
    );
  }

  return (
    <>
      <UsersPerTeam teams={all} onTeam={onTeam} />
      {/* The grid's own header row, so its filters and export don't sit alone in an empty band. */}
      <Panel
        title="Teams"
        subtitle={plural(teams.length, 'team')}
        flush
        actions={
          <>
            <StarredFilter kind="team" pressed={starred.on} onChange={starred.set} />
            {focus ? (
              <ChipToggle pressed onChange={() => setFocus(null)}>
                Only {focus.displayName}
              </ChipToggle>
            ) : null}
            <ExportCsvButton filename="teams" columns={teamColumns(spend)} rows={teams} />
          </>
        }
      >
        <TileGrid as="ul" label="Teams" cols={{ base: 1, lg: 2, xl: 3 }}>
          {teams.map((t) => {
            const counter = spend.get(t.name);
            const spent = counter ? Number(counter.spent) : 0;
            const amount = counter ? Number(counter.amount) : t.monthlyBudget ? Number(t.monthlyBudget) : 0;
            const ratio = amount > 0 ? spent / amount : 0;
            return (
              <li
                key={t.name}
                className="flex min-w-0 flex-col gap-3 bg-raised px-5 py-4"
                aria-label={t.displayName}
              >
                <div className="flex items-start gap-2.5">
                  <ColorChip color={t.color} className="mt-1.5 h-2.5 w-2.5" />
                  <div className="min-w-0 flex-1">
                    <h3 className="text-body font-semibold text-fg">
                      <Truncate text={t.displayName} />
                    </h3>
                    <p className="truncate text-caption text-muted">
                      <span className="mono">{t.name}</span> · {t.organization}
                      {t.team.metadata.created_at
                        ? ` · created ${formatDate(t.team.metadata.created_at, timeZone)}`
                        : ''}
                    </p>
                  </div>
                  {t.team.spec.enabled === false ? <Badge dot>Disabled</Badge> : null}
                  <TeamLinkButton name={t.name} label={t.displayName} />
                  <FavouriteButton
                    kind="team"
                    name={t.name}
                    label={`team ${t.displayName}`}
                    className="-mt-1 -mr-1"
                  />
                </div>
                <dl className="grid grid-cols-3 gap-2 text-caption">
                  <div>
                    <dt className="text-muted">Lead</dt>
                    <dd className="truncate text-fg">{t.lead ?? 'None'}</dd>
                  </div>
                  <div>
                    <dt className="text-muted">Members</dt>
                    <dd className="mono text-fg">{t.members.length}</dd>
                  </div>
                  <div>
                    <dt className="text-muted">Keys</dt>
                    <dd className="mono text-fg">{t.keys}</dd>
                  </div>
                </dl>
                <div>
                  {amount > 0 ? (
                    <>
                      <Meter
                        value={spent}
                        max={amount}
                        tone={ratio >= 1 ? 'error' : ratio >= 0.8 ? 'warn' : 'accent'}
                        label={`${t.displayName} spend`}
                      />
                      <p className="mt-1 text-caption text-muted">
                        <span className="mono">
                          {formatMoney(spent, { compact: true })} of {formatMoney(amount, { compact: true })}
                        </span>{' '}
                        · {formatPercent(ratio, 0)} this month
                      </p>
                    </>
                  ) : (
                    <p className="text-caption text-muted">
                      No budget{counter ? ` · ${formatMoney(spent)} spent` : ''}
                    </p>
                  )}
                </div>
                <div className="mt-auto flex items-center gap-1">
                  {t.members.length === 0 ? (
                    <span className="text-caption text-muted">No members</span>
                  ) : null}
                  {t.people.slice(0, 6).map((m) => (
                    <Avatar key={m.name} name={m.displayName} size="sm" />
                  ))}
                  {t.members.length > 6 ? (
                    <span className="text-caption text-muted">+{t.members.length - 6} more</span>
                  ) : null}
                  {t.members.length > 0 ? (
                    <Button size="sm" variant="ghost" onClick={() => onTeam(t.name)}>
                      View users
                    </Button>
                  ) : null}
                  <span className="ml-auto flex gap-1">
                    <Gate operation="replaceTeam" mode="hide">
                      <Button
                        size="sm"
                        variant="ghost"
                        icon="edit"
                        onClick={() => setEditing(t)}
                        aria-label={`Edit ${t.name}`}
                      >
                        Edit
                      </Button>
                    </Gate>
                    <Gate operation="deleteTeam" mode="hide">
                      <Button
                        size="sm"
                        variant="ghost"
                        icon="trash"
                        onClick={() => setDeleting(t)}
                        aria-label={`Delete ${t.name}`}
                      >
                        Delete
                      </Button>
                    </Gate>
                  </span>
                </div>
              </li>
            );
          })}
        </TileGrid>
      </Panel>
      {editing ? (
        <TeamFormDrawer editing={editing.team} onClose={() => setEditing(null)} onSaved={() => undefined} />
      ) : null}
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete team ${deleting?.name ?? ''}?`}
        confirmLabel="Delete team"
        typeToConfirm={deleting?.name}
        effect={
          deleting && deleting.projects.length > 0
            ? `The team still has ${plural(deleting.projects.length, 'project')} (${deleting.projects.join(', ')}). Move or delete them first: a team can't be deleted while something belongs to it.`
            : "The team and its policies are removed. This can't be undone."
        }
        onConfirm={async (reason) => {
          if (!deleting) return;
          await remove(deleting.name, reason);
          notify('success', 'Team deleted', deleting.name);
        }}
      />
    </>
  );
}
