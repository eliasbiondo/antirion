'use client';

import { useEffect, useState } from 'react';
import { z } from 'zod';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { BarRow } from '@/components/ui/charts';
import { Checkbox } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Textarea } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Panel, SummaryStrip, type SummaryItem } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Drawer } from '@/components/ui/overlay';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import type { Role } from '@/lib/api/types';
import { formatCount, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { actionVerb, groupPermissions, offboardedSince, permissionCatalogue } from '../facts';
import { useDirectory } from '../store';
import { ROLE_DESCRIPTIONS, roleLabel, type UserView } from '../types';

const LEVELS = ['gateway', 'organization', 'team', 'project'] as const;
type Level = (typeof LEVELS)[number];
const LEVEL_LABEL: Record<Level, string> = {
  gateway: 'Whole gateway',
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
};

const DAY = 86_400_000;

function holders(users: UserView[], role: string): number {
  return users.filter((u) => u.bindings.some((b) => b.role === role)).length;
}

/** Admins, joins and offboardings over 30 days, and the custom roles. */
function RoleSummary({ users, roles }: { users: UserView[]; roles: Role[] }) {
  const now = useNow(60_000);
  const offboardings = useDirectory((s) => s.offboardings);
  const load = useDirectory((s) => s.loadOffboardings);
  const canAudit = useCan('listAudit');
  useEffect(() => {
    if (canAudit) void load();
  }, [canAudit, load]);
  const admins = users.filter((u) =>
    u.bindings.some((b) => b.role === 'gateway_admin' || b.role === 'org_admin' || b.role === 'team_admin'),
  ).length;
  const joined = users.filter(
    (u) => u.user?.metadata.created_at && now - new Date(u.user.metadata.created_at).getTime() < 30 * DAY,
  ).length;
  const items: SummaryItem[] = [
    { label: 'Admins', value: admins, title: 'Gateway, organization and team admins' },
    { label: 'Custom roles', value: roles.filter((r) => !r.builtin).length },
    { label: 'New in the last 30 days', value: joined, title: 'Users created in the last 30 days' },
  ];
  if (canAudit)
    items.push({
      label: 'Offboarded in the last 30 days',
      value: offboardings.data ? offboardedSince(offboardings.data, now) : '—',
      title: 'Users offboarded or deleted, from the audit log',
    });
  return <SummaryStrip label="Role summary" items={items} />;
}

/** Roles: who holds each, what each allows and where it can be given; custom roles are edited here. */
export function RolesPanel({ users }: { users: UserView[] }) {
  const roles = useDirectory((s) => s.roles);
  const canWrite = useCan('patchAuthSettings');
  const [editing, setEditing] = useState<Role | 'new' | null>(null);
  const [deleting, setDeleting] = useState<Role | null>(null);
  const saveRole = useDirectory((s) => s.saveRole);
  const list = roles.data ?? [];
  const distribution = list
    .map((r) => ({ name: roleLabel(r.name), value: holders(users, r.name) }))
    .filter((r) => r.value > 0)
    .sort((a, b) => b.value - a.value);
  return (
    <>
      {roles.data ? <RoleSummary users={users} roles={list} /> : null}
      {distribution.length > 0 ? (
        <Panel
          title="Who holds each role"
          subtitle="A user may hold several roles."
          className="border-b border-line"
        >
          <ul aria-label="Users per role" className="grid grid-cols-1 gap-x-8 gap-y-1 lg:grid-cols-2">
            {distribution.map((r) => (
              <BarRow
                key={r.name}
                name={<span className="truncate">{r.name}</span>}
                value={r.value}
                max={distribution[0]?.value ?? 1}
                display={<span className="inline-block min-w-16">{plural(r.value, 'user')}</span>}
                label={`${r.name}: ${plural(r.value, 'user')}`}
              />
            ))}
          </ul>
        </Panel>
      ) : null}
      <Panel
        title="Roles"
        subtitle="Assign them on a user's page."
        bodyClassName="px-0 pb-0"
        actions={
          <Gate operation="patchAuthSettings" mode="hide">
            <Button size="sm" icon="plus" onClick={() => setEditing('new')}>
              Create role
            </Button>
          </Gate>
        }
      >
        {roles.error ? <ErrorState error={roles.error} subject="Roles" size="sm" /> : null}
        {roles.status === 'loading' && !roles.data ? <LoadingRows rows={4} /> : null}
        {roles.data ? (
          <Table label="Roles">
            <thead>
              <tr>
                <Th>Role</Th>
                <Th>What it allows</Th>
                <Th>Can be given for</Th>
                <Th numeric>Users</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {list.map((r) => {
                const count = holders(users, r.name);
                const items: MenuItem[] = [
                  { label: 'Edit', icon: 'edit', onSelect: () => setEditing(r) },
                  {
                    label: 'Delete',
                    icon: 'trash',
                    danger: true,
                    disabled: count > 0,
                    disabledReason: `${plural(count, 'user')} ${count === 1 ? 'holds' : 'hold'} it. Remove it from them first.`,
                    onSelect: () => setDeleting(r),
                  },
                ];
                return (
                  <Tr key={r.name}>
                    <Td className="font-semibold" title={r.name}>
                      <span className="flex items-center gap-2">
                        {roleLabel(r.name)}
                        {r.builtin ? null : <Badge tone="info">Custom</Badge>}
                      </span>
                    </Td>
                    <Td muted className="whitespace-normal">
                      {r.builtin
                        ? (ROLE_DESCRIPTIONS[r.name] ??
                          r.description ??
                          plural(r.permissions.length, 'permission'))
                        : (r.description ?? '')}
                      {r.builtin ? null : (
                        <Tooltip content={r.permissions.join(', ')}>
                          <span className="block text-caption text-dim">
                            {groupPermissions(r.permissions)
                              .map((g) => `${g.label}: ${g.permissions.map(actionVerb).join(', ')}`)
                              .join(' · ')}
                          </span>
                        </Tooltip>
                      )}
                    </Td>
                    <Td muted>{r.bindable_at.map((b) => LEVEL_LABEL[b]).join(', ')}</Td>
                    <Td numeric>{formatCount(count)}</Td>
                    <Td className="text-right">
                      {!r.builtin && canWrite ? (
                        <MenuButton items={items} iconOnly label={`Actions for ${roleLabel(r.name)}`} />
                      ) : null}
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : null}
      </Panel>
      {editing ? (
        <RoleFormDrawer
          editing={editing === 'new' ? null : editing}
          roles={list}
          onClose={() => setEditing(null)}
        />
      ) : null}
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete role ${deleting ? roleLabel(deleting.name) : ''}?`}
        confirmLabel="Delete role"
        objectName={deleting?.name}
        effect="It can no longer be given to anyone. Nobody holds it now."
        onConfirm={async (reason) => {
          if (!deleting) return;
          await saveRole(deleting.name, null, reason);
          notify('success', 'Role deleted', roleLabel(deleting.name));
        }}
      />
    </>
  );
}

const roleSchema = z.object({
  name: z
    .string()
    .trim()
    .regex(/^[a-z][a-z0-9_]{0,63}$/, 'Start with a letter; then lowercase letters, numbers and _ (up to 64)'),
  description: z.string().trim().max(500, 'At most 500 characters'),
  levels: z.array(z.enum(LEVELS)).min(1, 'Choose at least one place it can be given for'),
  permissions: z.array(z.string()).min(1, 'Choose at least one thing it allows'),
});

/** Creates or edits a custom role: name, description, where it can be given and what it allows. */
export function RoleFormDrawer({
  editing,
  roles,
  onClose,
}: {
  editing: Role | null;
  roles: Role[];
  onClose: () => void;
}) {
  const saveRole = useDirectory((s) => s.saveRole);
  const catalogue = groupPermissions(permissionCatalogue(roles));
  const [values, setValues] = useState(() => ({
    name: editing?.name ?? '',
    description: editing?.description ?? '',
    levels: editing?.bindable_at ?? (['organization', 'team', 'project'] satisfies Level[]),
    permissions: editing?.permissions ?? [],
  }));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const has = (p: string) => values.permissions.includes(p);
  const toggle = (list: string[], on: boolean) =>
    setValues((v) => ({
      ...v,
      permissions: on
        ? [...new Set([...v.permissions, ...list])]
        : v.permissions.filter((p) => !list.includes(p)),
    }));

  async function submit() {
    const result = validate(roleSchema, values);
    const taken = !editing && roles.some((r) => r.name === values.name.trim());
    if (!result.ok || taken) {
      setErrors({
        ...(result.ok ? {} : result.errors),
        ...(taken ? { name: 'A role with this name exists' } : {}),
      });
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const v = result.data;
    try {
      await saveRole(v.name, {
        permissions: v.permissions,
        bindable_at: v.levels,
        ...(v.description ? { description: v.description } : {}),
      });
      notify('success', editing ? 'Role saved' : 'Role created', roleLabel(v.name));
      onClose();
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `auth.admin.roles.${v.name}`));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      open
      width="lg"
      onClose={onClose}
      title={editing ? `Edit ${roleLabel(editing.name)}` : 'Create role'}
      subtitle="A role is a set of permissions you can give to people on their page."
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {editing ? 'Save changes' : 'Create role'}
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
        <Field
          label="Name"
          required
          error={errors.name}
          hint="Lowercase letters, numbers and _. Shown as words."
        >
          <Input
            mono
            value={values.name}
            readOnly={Boolean(editing)}
            onChange={(e) => setValues({ ...values, name: e.target.value })}
            placeholder="e.g. finance_viewer"
            data-autofocus
          />
        </Field>
        <Field label="Description" optional error={errors.description}>
          <Textarea
            rows={2}
            value={values.description}
            onChange={(e) => setValues({ ...values, description: e.target.value })}
            placeholder="e.g. Reads spend and usage, without prompts"
          />
        </Field>
        <Field label="Can be given for" error={errors.levels}>
          <div className="flex flex-wrap gap-x-5 gap-y-2">
            {LEVELS.map((l) => (
              <Checkbox
                key={l}
                label={LEVEL_LABEL[l]}
                checked={values.levels.includes(l)}
                onChange={(on) =>
                  setValues({
                    ...values,
                    levels: on ? [...values.levels, l] : values.levels.filter((x) => x !== l),
                  })
                }
              />
            ))}
          </div>
        </Field>
        <Field
          label="What it allows"
          error={errors.permissions}
          hint={`${plural(values.permissions.length, 'permission')} chosen`}
        >
          <ul className="flex flex-col divide-y divide-line rounded-control border border-line">
            {catalogue.map((g) => {
              const on = g.permissions.filter(has).length;
              return (
                <li
                  key={g.resource}
                  className="grid grid-cols-[180px_1fr] gap-3 px-3 py-2 max-sm:grid-cols-1"
                >
                  <Checkbox
                    label={g.label}
                    checked={on === 0 ? false : on === g.permissions.length ? true : 'mixed'}
                    onChange={(next) => toggle(g.permissions, next)}
                  />
                  <div className="flex flex-wrap gap-x-4 gap-y-1.5">
                    {g.permissions.map((p) => (
                      <Tooltip key={p} content={p}>
                        <span>
                          <Checkbox
                            label={actionVerb(p)}
                            checked={has(p)}
                            onChange={(next) => toggle([p], next)}
                            aria-label={`${g.label}: ${actionVerb(p)}`}
                          />
                        </span>
                      </Tooltip>
                    ))}
                  </div>
                </li>
              );
            })}
          </ul>
        </Field>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}
