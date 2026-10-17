'use client';

import { useState } from 'react';
import { z } from 'zod';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Dialog } from '@/components/ui/overlay';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatDate } from '@/lib/time';
import { resourceName, validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { directoryApi } from '../api';
import { setLevelEnabled, useDirectory, useOrganizations, useProjects, useTeams } from '../store';
import { cascadeOf, cascadeText, disableEffect, TenancyDrawer } from './tenancy-detail';

type Kind = 'organization' | 'project';

const createSchema = z.object({
  name: resourceName,
  displayName: z.string().trim().max(200),
  parent: z.string(),
});

function CreateDialog({ kind, onClose }: { kind: Kind; onClose: () => void }) {
  const teams = useTeams((s) => s.items);
  const createOrg = useOrganizations((s) => s.create);
  const createProject = useProjects((s) => s.create);
  const [values, setValues] = useState({ name: '', displayName: '', parent: teams[0]?.name ?? 'default' });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  async function submit() {
    const result = validate(createSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      if (kind === 'organization')
        await createOrg({
          name: values.name,
          spec: values.displayName ? { display_name: values.displayName } : {},
        });
      else await createProject({ name: values.name, spec: { team: values.parent } });
      notify('success', kind === 'organization' ? 'Organization created' : 'Project created', values.name);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      open
      onClose={onClose}
      title={kind === 'organization' ? 'Create organization' : 'Create project'}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Create
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        <Field label="Name" error={errors.name} required>
          <Input
            mono
            value={values.name}
            onChange={(e) => setValues({ ...values, name: e.target.value })}
            data-autofocus
          />
        </Field>
        {kind === 'organization' ? (
          <Field label="Display name" optional>
            <Input
              value={values.displayName}
              onChange={(e) => setValues({ ...values, displayName: e.target.value })}
            />
          </Field>
        ) : (
          <Field label="Team" required>
            <Combobox
              value={values.parent}
              onChange={(v) => setValues({ ...values, parent: v })}
              options={teams.map((t) => ({ value: t.name, label: t.name }))}
            />
          </Field>
        )}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Dialog>
  );
}

function RowMenu({
  name,
  enabled,
  canToggle,
  canDelete,
  onOpen,
  onToggle,
  onDelete,
}: {
  name: string;
  enabled: boolean;
  canToggle: boolean;
  canDelete: boolean;
  onOpen: () => void;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const items: MenuItem[] = [{ label: 'Open details', icon: 'eye', onSelect: onOpen }];
  if (canToggle)
    items.push({
      label: enabled ? 'Disable' : 'Enable',
      icon: enabled ? 'pause' : 'play',
      onSelect: onToggle,
    });
  if (canDelete)
    items.push({
      label: 'Delete',
      icon: 'trash',
      danger: true,
      disabled: name === 'default',
      disabledReason: 'The default organization, team and project always exist.',
      onSelect: onDelete,
    });
  return <MenuButton items={items} iconOnly label={`Actions for ${name}`} />;
}

/** The hierarchy organization → team → project, with create, disable or enable, and delete. */
export function OrganizationsPanel({ show }: { show: 'organizations' | 'projects' }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const orgs = useOrganizations();
  const projects = useProjects();
  const teams = useTeams((s) => s.items);
  const keys = useDirectory((s) => s.keys.data) ?? [];
  const [creating, setCreating] = useState<Kind | null>(null);
  const [toggling, setToggling] = useState<{ kind: Kind; name: string; enabled: boolean } | null>(null);
  const [deleting, setDeleting] = useState<{ kind: Kind; name: string } | null>(null);
  const canPatchOrg = useCan('patchOrganization');
  const canDeleteOrg = useCan('deleteOrganization');
  const canPatchProject = useCan('patchProject');
  const canDeleteProject = useCan('deleteProject');
  // `org` is the scope of every page, so the open level is `detail`.
  const [detail, setDetail] = useQueryParam('detail');
  const open: { kind: Kind; name: string } | null = detail
    ? { kind: show === 'organizations' ? 'organization' : 'project', name: detail }
    : null;
  const openLevel = (_kind: Kind, name: string) => setDetail(name);
  const closeLevel = () => setDetail(null);
  const below = (kind: Kind, name: string) => cascadeOf(kind, name, teams, projects.items, keys);
  const isEnabled = (kind: Kind, name: string) =>
    (kind === 'organization'
      ? orgs.items.find((o) => o.name === name)?.spec.enabled
      : projects.items.find((p) => p.name === name)?.spec.enabled) !== false;
  const deletingBelow = deleting ? below(deleting.kind, deleting.name) : null;
  const deletingText = deletingBelow ? cascadeText(deletingBelow) : null;

  return (
    <div className="flex flex-col">
      {show === 'organizations' ? (
        <Section
          flush
          title="Organizations"
          subtitle="Each keeps its own teams, projects, keys and settings."
          actions={
            <Gate operation="createOrganization" mode="hide">
              <Button size="sm" icon="plus" onClick={() => setCreating('organization')}>
                Create organization
              </Button>
            </Gate>
          }
        >
          {orgs.status === 'loading' && orgs.items.length === 0 ? <LoadingRows rows={2} /> : null}
          {orgs.error ? <ErrorState error={orgs.error} subject="Organizations" /> : null}
          <Table label="Organizations">
            <thead>
              <tr>
                <Th>Name</Th>
                <Th>Display name</Th>
                <Th>Teams</Th>
                <Th>State</Th>
                <Th>Created</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {orgs.items.map((o) => (
                <Tr key={o.name}>
                  <Td mono>
                    <button
                      type="button"
                      className="text-fg hover:underline"
                      onClick={() => openLevel('organization', o.name)}
                    >
                      {o.name}
                    </button>
                  </Td>
                  <Td>{o.spec.display_name ?? '—'}</Td>
                  <Td mono>{teams.filter((t) => t.spec.organization === o.name).length}</Td>
                  <Td>
                    {o.spec.enabled === false ? (
                      <Badge dot>Disabled</Badge>
                    ) : (
                      <Badge tone="success" dot>
                        Enabled
                      </Badge>
                    )}
                  </Td>
                  <Td muted>{formatDate(o.metadata.created_at, timeZone)}</Td>
                  <Td className="text-right">
                    <RowMenu
                      name={o.name}
                      enabled={o.spec.enabled !== false}
                      canToggle={canPatchOrg}
                      canDelete={canDeleteOrg}
                      onOpen={() => openLevel('organization', o.name)}
                      onToggle={() =>
                        setToggling({ kind: 'organization', name: o.name, enabled: o.spec.enabled !== false })
                      }
                      onDelete={() => setDeleting({ kind: 'organization', name: o.name })}
                    />
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        </Section>
      ) : (
        <Section
          flush
          title="Projects"
          subtitle="Keys belong to a project, and a project to a team."
          actions={
            <Gate operation="createProject" mode="hide">
              <Button size="sm" icon="plus" onClick={() => setCreating('project')}>
                Create project
              </Button>
            </Gate>
          }
        >
          {projects.error ? <ErrorState error={projects.error} subject="Projects" /> : null}
          <Table label="Projects">
            <thead>
              <tr>
                <Th>Name</Th>
                <Th>Team</Th>
                <Th>Keys</Th>
                <Th>State</Th>
                <Th>Created</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {projects.items.map((p) => (
                <Tr key={p.name}>
                  <Td mono>
                    <button
                      type="button"
                      className="text-fg hover:underline"
                      onClick={() => openLevel('project', p.name)}
                    >
                      {p.name}
                    </button>
                  </Td>
                  <Td mono muted>
                    {p.spec.team}
                  </Td>
                  <Td mono>{keys.filter((k) => (k.spec.project ?? 'default') === p.name).length}</Td>
                  <Td>
                    {p.spec.enabled === false ? (
                      <Badge dot>Disabled</Badge>
                    ) : (
                      <Badge tone="success" dot>
                        Enabled
                      </Badge>
                    )}
                  </Td>
                  <Td muted>{formatDate(p.metadata.created_at, timeZone)}</Td>
                  <Td className="text-right">
                    <RowMenu
                      name={p.name}
                      enabled={p.spec.enabled !== false}
                      canToggle={canPatchProject}
                      canDelete={canDeleteProject}
                      onOpen={() => openLevel('project', p.name)}
                      onToggle={() =>
                        setToggling({ kind: 'project', name: p.name, enabled: p.spec.enabled !== false })
                      }
                      onDelete={() => setDeleting({ kind: 'project', name: p.name })}
                    />
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        </Section>
      )}
      {creating ? <CreateDialog kind={creating} onClose={() => setCreating(null)} /> : null}
      {open ? (
        <TenancyDrawer
          kind={open.kind}
          name={open.name}
          onClose={closeLevel}
          onToggle={() =>
            setToggling({ kind: open.kind, name: open.name, enabled: isEnabled(open.kind, open.name) })
          }
          onDelete={() => setDeleting({ kind: open.kind, name: open.name })}
        />
      ) : null}
      <ConfirmDialog
        open={toggling !== null}
        onClose={() => setToggling(null)}
        title={
          toggling ? `${toggling.enabled ? 'Disable' : 'Enable'} ${toggling.kind} ${toggling.name}?` : ''
        }
        tone="primary"
        confirmLabel={`${toggling?.enabled ? 'Disable' : 'Enable'} ${toggling?.kind ?? ''}`}
        effect={
          toggling?.enabled
            ? disableEffect(toggling.kind, below(toggling.kind, toggling.name))
            : 'Requests through it are accepted again, and its keys work as before.'
        }
        onConfirm={async (reason) => {
          if (!toggling) return;
          await setLevelEnabled(toggling.kind, toggling.name, !toggling.enabled, reason);
          notify('success', toggling.enabled ? 'Disabled' : 'Enabled', toggling.name);
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={deleting ? `Delete ${deleting.kind} ${deleting.name}?` : ''}
        confirmLabel={`Delete ${deleting?.kind ?? ''}`}
        typeToConfirm={deleting?.kind === 'organization' || deletingText ? deleting?.name : undefined}
        objectName={deleting?.name}
        effect={
          deleting && deletingBelow
            ? deletingText
              ? `Everything in it is deleted too: ${deletingText}. Apps using those keys stop working at once. This can't be undone.`
              : "Nothing is in it. This can't be undone."
            : ''
        }
        onConfirm={async (reason) => {
          if (!deleting || !deletingBelow) return;
          const current =
            deleting.kind === 'organization'
              ? useOrganizations.getState().byName(deleting.name)
              : useProjects.getState().byName(deleting.name);
          await directoryApi.deleteLevel(
            deleting.kind,
            deleting.name,
            Boolean(deletingText),
            current?.metadata.etag,
            reason,
          );
          if (open?.name === deleting.name) closeLevel();
          await Promise.all([
            useOrganizations.getState().fetch({ quiet: true }),
            useTeams.getState().fetch({ quiet: true }),
            useProjects.getState().fetch({ quiet: true }),
            useDirectory.getState().load(),
          ]);
          notify(
            'success',
            deleting.kind === 'organization' ? 'Organization deleted' : 'Project deleted',
            deleting.name,
          );
        }}
      />
    </div>
  );
}

/**
 * The Create organization dialog on its own, for the organization switcher's "Create organization"
 * (the top bar renders it; SH-07).
 */
export function CreateOrganizationDialog({ onClose }: { onClose: () => void }) {
  return <CreateDialog kind="organization" onClose={onClose} />;
}
