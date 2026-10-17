'use client';

import Link from 'next/link';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Gate } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailSection } from '@/components/ui/list-detail';
import { ColorChip, CopyLinkButton } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState } from '@/components/ui/states';
import type { KeyResource, ProjectResource, TeamResource } from '@/lib/api/types';
import { plural } from '@/lib/format';
import { formatDate } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { useDirectory, useOrganizations, useProjects, useTeams, useUsers } from '../store';
import { roleLabel } from '../types';

export type Level = 'organization' | 'project';

export interface Cascade {
  teams: string[];
  projects: string[];
  keys: string[];
}

/** What sits below an organization or a project: what disabling stops and a delete removes. */
export function cascadeOf(
  kind: Level,
  name: string,
  teams: TeamResource[],
  projects: ProjectResource[],
  keys: KeyResource[],
): Cascade {
  const teamNames =
    kind === 'organization' ? teams.filter((t) => t.spec.organization === name).map((t) => t.name) : [];
  const projectNames =
    kind === 'organization'
      ? projects.filter((p) => teamNames.includes(p.spec.team)).map((p) => p.name)
      : [name];
  const keyNames = keys.filter((k) => projectNames.includes(k.spec.project ?? 'default')).map((k) => k.name);
  return {
    teams: teamNames,
    projects: kind === 'organization' ? projectNames : [],
    keys: keyNames,
  };
}

/** "2 teams, 5 projects and 9 keys", or null when nothing is below. */
export function cascadeText(c: Cascade): string | null {
  const parts = [
    c.teams.length ? plural(c.teams.length, 'team') : null,
    c.projects.length ? plural(c.projects.length, 'project') : null,
    c.keys.length ? plural(c.keys.length, 'key') : null,
  ].filter((p): p is string => p !== null);
  if (parts.length === 0) return null;
  return parts.length === 1 ? (parts[0] ?? null) : `${parts.slice(0, -1).join(', ')} and ${parts.at(-1)}`;
}

/** What a disable does to the apps of a level, in words. */
export function disableEffect(kind: Level, c: Cascade): string {
  const keys = c.keys.length
    ? `Apps using its ${plural(c.keys.length, 'key')} are refused with "${kind} disabled"`
    : 'Requests through it are refused';
  return `${keys} until you enable it again${kind === 'organization' && c.teams.length ? `, across ${plural(c.teams.length, 'team')}` : ''}. Nothing is deleted, and people can still sign in to the console.`;
}

function NameList({ names, href }: { names: string[]; href: (n: string) => string }) {
  return (
    <ul className="mono flex flex-wrap gap-1.5 text-caption">
      {names.map((n) => (
        <li key={n}>
          <Link href={href(n)} className="rounded-xs border border-line px-1.5 py-0.5 text-fg hover:bg-hover">
            {n}
          </Link>
        </li>
      ))}
    </ul>
  );
}

/** An organization or project: its facts, what is below it, who holds roles on it, and its actions. */
export function TenancyDrawer({
  kind,
  name,
  onClose,
  onToggle,
  onDelete,
}: {
  kind: Level;
  name: string;
  onClose: () => void;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const org = useOrganizations((s) => s.items.find((o) => o.name === name));
  const project = useProjects((s) => s.items.find((p) => p.name === name));
  const teams = useTeams((s) => s.items);
  const projects = useProjects((s) => s.items);
  const users = useUsers((s) => s.items);
  const keys = useDirectory((s) => s.keys.data) ?? [];
  const bindings = useDirectory((s) => s.bindings.data) ?? [];
  const resource = kind === 'organization' ? org : project;
  const cascade = cascadeOf(kind, name, teams, projects, keys);
  const enabled = resource?.spec.enabled !== false;
  const scope = `${kind}/${name}`;
  const holders = bindings.filter((b) => b.scope === scope);
  const team = kind === 'project' ? teams.find((t) => t.name === project?.spec.team) : undefined;
  const people =
    kind === 'organization'
      ? users.filter((u) => cascade.teams.includes(u.spec.primary_team ?? '')).length
      : null;
  const title =
    kind === 'organization' ? `Organization ${org?.spec.display_name ?? name}` : `Project ${name}`;

  return (
    <Drawer
      open
      width="lg"
      onClose={onClose}
      headerActions={<CopyLinkButton />}
      title={title}
      subtitle={
        <span className="flex flex-wrap items-center gap-2">
          <span className="mono">{name}</span>
          {enabled ? (
            <Badge tone="success" dot>
              Enabled
            </Badge>
          ) : (
            <Badge dot>Disabled</Badge>
          )}
          {resource ? <span>Created {formatDate(resource.metadata.created_at, timeZone)}</span> : null}
        </span>
      }
      footer={
        <>
          <Gate operation={kind === 'organization' ? 'deleteOrganization' : 'deleteProject'} mode="hide">
            <Button
              variant="ghost"
              icon="trash"
              disabled={name === 'default'}
              disabledReason={`The default ${kind} always exists.`}
              onClick={onDelete}
            >
              Delete
            </Button>
          </Gate>
          <Gate operation={kind === 'organization' ? 'patchOrganization' : 'patchProject'} mode="hide">
            <Button variant="secondary" icon={enabled ? 'pause' : 'play'} onClick={onToggle}>
              {enabled ? `Disable ${kind}` : `Enable ${kind}`}
            </Button>
          </Gate>
        </>
      }
    >
      {!resource ? (
        <EmptyState title={`No ${kind} named ${name}`} description="It may have been deleted." />
      ) : (
        <div className="-mx-5 -my-4">
          <DetailSection title="Details">
            <DefinitionGrid
              columns={3}
              items={
                kind === 'organization'
                  ? [
                      { label: 'Teams', value: cascade.teams.length },
                      { label: 'Projects', value: cascade.projects.length },
                      { label: 'Keys', value: cascade.keys.length },
                      { label: 'People', value: people ?? 0 },
                      { label: 'Roles given here', value: holders.length },
                      {
                        label: 'Longest key lifetime',
                        value: org?.spec.max_key_lifetime ?? 'No limit',
                      },
                    ]
                  : [
                      {
                        label: 'Team',
                        value: (
                          <span className="flex items-center gap-1.5">
                            <ColorChip color={team?.spec.color} className="h-2 w-2" />
                            {project?.spec.team}
                          </span>
                        ),
                      },
                      { label: 'Organization', value: team?.spec.organization ?? '—' },
                      { label: 'Keys', value: cascade.keys.length },
                    ]
              }
            />
          </DetailSection>
          {kind === 'organization' ? (
            <DetailSection title="Teams">
              {cascade.teams.length === 0 ? (
                <EmptyState size="sm" title="No teams yet" />
              ) : (
                <NameList names={cascade.teams} href={() => '/teams/?tab=teams'} />
              )}
            </DetailSection>
          ) : null}
          {kind === 'organization' ? (
            <DetailSection title="Projects">
              {cascade.projects.length === 0 ? (
                <EmptyState size="sm" title="No projects yet" />
              ) : (
                <NameList
                  names={cascade.projects}
                  href={(n) => `/teams/?tab=projects&detail=${encodeURIComponent(n)}`}
                />
              )}
            </DetailSection>
          ) : null}
          <DetailSection title="Keys">
            {cascade.keys.length === 0 ? (
              <EmptyState size="sm" title="No keys" />
            ) : (
              <NameList names={cascade.keys} href={(n) => `/keys/?key=${encodeURIComponent(n)}`} />
            )}
          </DetailSection>
          <DetailSection title="Roles given here">
            {holders.length === 0 ? (
              <p className="text-ui text-muted">Nobody holds a role on this {kind}.</p>
            ) : (
              <ul className="flex flex-col gap-1 text-ui">
                {holders.map((b) => (
                  <li key={b.id} className="flex items-center gap-2">
                    <Badge tone="warn">{roleLabel(b.role)}</Badge>
                    {b.principal.type === 'user' ? (
                      <Link
                        href={`/teams/?user=${encodeURIComponent(b.principal.id)}`}
                        className="text-fg hover:underline"
                      >
                        {b.principal.id}
                      </Link>
                    ) : (
                      <span className="text-muted">token {b.principal.id}</span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </DetailSection>
          <DetailSection title={enabled ? 'If you disable it' : 'While it is disabled'}>
            <p className="text-ui text-muted">{disableEffect(kind, cascade)}</p>
          </DetailSection>
          <DetailSection title="If you delete it">
            <p className="text-ui text-muted">
              {cascadeText(cascade)
                ? `Its ${cascadeText(cascade)} are deleted with it, and the keys stop working at once. Spend, logs and the audit log keep their names.`
                : 'Nothing is below it. Spend, logs and the audit log keep its name.'}
            </p>
          </DetailSection>
        </div>
      )}
    </Drawer>
  );
}
