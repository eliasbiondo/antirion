'use client';

import { useState } from 'react';
import { z } from 'zod';

import { Button } from '@/components/ui/button';
import { Checkbox, Combobox, Field, Input, Select } from '@/components/ui/form';
import { CancelButton, Dialog, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { TeamResource, TeamSpec, UserResource, UserSpec } from '@/lib/api/types';
import { money, resourceName, serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { useDirectory, useOrganizations, useProjects, useTeams, useUsers } from '../store';
import { ROLE_DESCRIPTIONS, roleLabel } from '../types';
import { useEdited } from '@/lib/hooks';

const email = z
  .string()
  .trim()
  .regex(/^[^@\s]+@[^@\s]+$/, 'Enter a valid email address');

const userSchema = z
  .object({
    name: resourceName,
    displayName: z.string().trim().max(200, 'At most 200 characters'),
    email: z.string().trim(),
    title: z.string().trim().max(100, 'At most 100 characters'),
    kind: z.enum(['human', 'service']),
    primaryTeam: z.string(),
    enabled: z.boolean(),
    role: z.string(),
    scope: z.string(),
  })
  .superRefine((v, ctx) => {
    if (v.kind === 'human' && !email.safeParse(v.email).success)
      ctx.addIssue({ code: 'custom', path: ['email'], message: 'People need a valid email' });
  });

type UserValues = z.input<typeof userSchema>;

/** Creates a user (with a first role binding) or edits one. */
export function UserFormDrawer({
  editing,
  kind = 'human',
  onClose,
  onSaved,
}: {
  editing?: UserResource | undefined;
  /** The type a new account starts with ("Create service account" opens it as a service). */
  kind?: 'human' | 'service';
  onClose: () => void;
  onSaved: (name: string) => void;
}) {
  const teams = useTeams((s) => s.items);
  const roles = useDirectory((s) => s.roles.data) ?? [];
  const create = useUsers((s) => s.create);
  const update = useUsers((s) => s.update);
  const [values, setValues] = useState<UserValues>(() => ({
    name: editing?.name ?? '',
    displayName: editing?.spec.display_name ?? '',
    email: editing?.spec.email ?? '',
    title: editing?.spec.title ?? '',
    kind: editing?.spec.kind ?? kind,
    primaryTeam: editing?.spec.primary_team ?? '',
    enabled: editing?.spec.enabled !== false,
    role: 'developer',
    scope: teams[0] ? `team/${teams[0].name}` : 'organization/default',
  }));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const set = <K extends keyof UserValues>(key: K, value: UserValues[K]) =>
    setValues((v) => ({ ...v, [key]: value }));

  async function submit() {
    const result = validate(userSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const v = result.data;
    const spec: UserSpec = {
      ...(editing?.spec ?? {}),
      kind: v.kind,
      enabled: v.enabled,
      ...(v.email ? { email: v.email } : {}),
      ...(v.displayName ? { display_name: v.displayName } : {}),
      ...(v.title ? { title: v.title } : {}),
      ...(v.primaryTeam ? { primary_team: v.primaryTeam } : {}),
    };
    if (!v.displayName) delete spec.display_name;
    if (!v.title) delete spec.title;
    if (!v.primaryTeam) delete spec.primary_team;
    if (!editing && v.role) spec.role_bindings = [{ role: v.role, scope: v.scope }];
    try {
      if (editing) await update(editing.name, spec);
      else await create({ name: v.name, spec });
      notify('success', editing ? 'User saved' : 'User added', v.name);
      onSaved(v.name);
      onClose();
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `users.${v.name}`));
    } finally {
      setBusy(false);
    }
  }

  const scopes = [
    { value: 'organization/default', label: 'organization · default' },
    ...teams.map((t) => ({ value: `team/${t.name}`, label: `team · ${t.name}` })),
  ];

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title={
        editing
          ? `Edit ${editing.name}`
          : kind === 'service'
            ? 'Create service account'
            : 'Add user without inviting'
      }
      subtitle={
        editing
          ? "Roles are assigned on the user's page."
          : 'Creates the account right away, with no email. To send a sign-in link, invite them instead.'
      }
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {editing ? 'Save changes' : 'Add user'}
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
        <div className="grid grid-cols-2 gap-4">
          <Field
            label="Username"
            error={errors.name}
            required
            hint="Lowercase letters, numbers, - and _. Shown in the audit log."
          >
            <Input
              mono
              value={values.name}
              readOnly={Boolean(editing)}
              onChange={(e) => set('name', e.target.value)}
              data-autofocus
            />
          </Field>
          <Field label="Type">
            <Select
              value={values.kind}
              onChange={(v) => set('kind', v === 'service' ? 'service' : 'human')}
              options={[
                { value: 'human', label: 'Person' },
                { value: 'service', label: 'Service account' },
              ]}
            />
          </Field>
          <Field label="Name" optional error={errors.displayName}>
            <Input
              value={values.displayName}
              onChange={(e) => set('displayName', e.target.value)}
              placeholder="e.g. Ada Lovelace"
            />
          </Field>
          <Field label="Email" error={errors.email} required={values.kind === 'human'}>
            <Input type="email" value={values.email} onChange={(e) => set('email', e.target.value)} />
          </Field>
          <Field label="Job title" optional error={errors.title}>
            <Input value={values.title} onChange={(e) => set('title', e.target.value)} />
          </Field>
          <Field label="Team" optional>
            <Combobox
              value={values.primaryTeam}
              onChange={(v) => set('primaryTeam', v)}
              emptyOption="None"
              options={teams.map((t) => ({ value: t.name, label: t.spec.display_name ?? t.name }))}
            />
          </Field>
        </div>
        {editing ? null : (
          <div className="grid grid-cols-2 gap-4">
            <Field label="Role">
              <Select
                value={values.role}
                onChange={(v) => set('role', v)}
                emptyOption="No role yet"
                options={roles.map((r) => ({ value: r.name, label: roleLabel(r.name) }))}
              />
            </Field>
            <Field label="Scope">
              <Select value={values.scope} onChange={(v) => set('scope', v)} options={scopes} />
            </Field>
          </div>
        )}
        <Checkbox
          label="Enabled"
          description="A disabled user cannot sign in and their sessions end."
          checked={values.enabled}
          onChange={(v) => set('enabled', v)}
        />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}

const teamSchema = z.object({
  name: resourceName,
  displayName: z.string().trim().max(200, 'At most 200 characters'),
  organization: resourceName,
  color: z.string().regex(/^#[0-9a-fA-F]{6}$/, 'A colour like #3366ff'),
  lead: z.string(),
  budget: z
    .string()
    .trim()
    .refine((v) => v === '' || money.safeParse(v).success, 'A monthly amount in USD, or empty'),
  mode: z.enum(['soft', 'hard']),
});

type TeamValues = z.input<typeof teamSchema>;

const COLORS = ['#3e72cb', '#2f9e6f', '#c9891a', '#cb3e3e', '#8b5cf6', '#0ea5a4', '#d97757', '#64748b'];

/** Creates or edits a team: display name, colour, lead, organization and a monthly budget. */
export function TeamFormDrawer({
  editing,
  onClose,
  onSaved,
}: {
  editing?: TeamResource | undefined;
  onClose: () => void;
  onSaved: (name: string) => void;
}) {
  const orgs = useOrganizations((s) => s.items);
  const users = useUsers((s) => s.items);
  const create = useTeams((s) => s.create);
  const update = useTeams((s) => s.update);
  const monthly = (
    editing?.spec.policies?.budgets as
      Record<string, { amount?: string | number; mode?: string } | undefined> | undefined
  )?.monthly;
  const [values, setValues] = useState<TeamValues>(() => ({
    name: editing?.name ?? '',
    displayName: editing?.spec.display_name ?? '',
    organization: editing?.spec.organization ?? orgs[0]?.name ?? 'default',
    color: editing?.spec.color ?? COLORS[0] ?? '#3e72cb',
    lead: editing?.spec.lead ?? '',
    budget: monthly?.amount !== undefined ? String(monthly.amount) : '',
    mode: monthly?.mode === 'hard' ? 'hard' : 'soft',
  }));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const set = <K extends keyof TeamValues>(key: K, value: TeamValues[K]) =>
    setValues((v) => ({ ...v, [key]: value }));

  async function submit() {
    const result = validate(teamSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const v = result.data;
    const budgets = { ...((editing?.spec.policies?.budgets ?? {}) as Record<string, unknown>) };
    if (v.budget)
      budgets.monthly = {
        unit: 'usd',
        amount: v.budget,
        period: 'monthly',
        mode: v.mode,
        alert_at: [0.5, 0.8, 1],
      };
    else delete budgets.monthly;
    const policies = { ...(editing?.spec.policies ?? {}), budgets } as TeamSpec['policies'];
    const spec: TeamSpec = {
      ...(editing?.spec ?? {}),
      organization: v.organization,
      color: v.color,
      ...(v.displayName ? { display_name: v.displayName } : {}),
      ...(v.lead ? { lead: v.lead } : {}),
      ...(Object.keys(budgets).length > 0 ? { policies } : {}),
    };
    if (!v.displayName) delete spec.display_name;
    if (!v.lead) delete spec.lead;
    if (Object.keys(budgets).length === 0 && spec.policies) delete spec.policies.budgets;
    try {
      if (editing) await update(editing.name, spec);
      else await create({ name: v.name, spec });
      await useProjects.getState().fetch({ quiet: true });
      notify('success', editing ? 'Team saved' : 'Team created', v.name);
      onSaved(v.name);
      onClose();
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `teams.${v.name}`));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title={editing ? `Edit ${editing.name}` : 'Create team'}
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {editing ? 'Save changes' : 'Create team'}
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
        <div className="grid grid-cols-2 gap-4">
          <Field label="Name" error={errors.name} required hint="Lowercase letters, numbers, - and _.">
            <Input
              mono
              value={values.name}
              readOnly={Boolean(editing)}
              onChange={(e) => set('name', e.target.value)}
              data-autofocus
            />
          </Field>
          <Field label="Display name" optional error={errors.displayName}>
            <Input
              value={values.displayName}
              onChange={(e) => set('displayName', e.target.value)}
              placeholder="e.g. Support AI"
            />
          </Field>
          <Field label="Organization" error={errors.organization} required>
            <Combobox
              value={values.organization}
              onChange={(v) => set('organization', v)}
              options={(orgs.length > 0 ? orgs.map((o) => o.name) : ['default']).map((o) => ({
                value: o,
                label: o,
              }))}
            />
          </Field>
          <Field label="Lead" optional>
            <Combobox
              value={values.lead}
              onChange={(v) => set('lead', v)}
              emptyOption="No lead"
              options={users.map((u) => ({ value: u.name, label: u.spec.display_name ?? u.name }))}
            />
          </Field>
        </div>
        <fieldset>
          <legend className="mb-2 text-ui font-medium text-fg">Colour</legend>
          <div className="flex gap-2" role="radiogroup" aria-label="Team colour">
            {COLORS.map((c) => (
              <button
                key={c}
                type="button"
                role="radio"
                aria-checked={values.color === c}
                aria-label={c}
                onClick={() => set('color', c)}
                ref={(el) => {
                  if (el) el.style.backgroundColor = c;
                }}
                className={`h-6 w-6 rounded-full border-2 ${values.color === c ? 'border-fg' : 'border-transparent'}`}
              />
            ))}
          </div>
        </fieldset>
        <div className="grid grid-cols-2 gap-4">
          <Field
            label="Monthly budget (USD)"
            optional
            error={errors.budget}
            hint="Leave blank for no team budget."
          >
            <Input
              mono
              inputMode="decimal"
              value={values.budget}
              onChange={(e) => set('budget', e.target.value)}
            />
          </Field>
          <Field label="When the budget is reached">
            <Select
              value={values.mode}
              onChange={(v) => set('mode', v === 'hard' ? 'hard' : 'soft')}
              options={[
                { value: 'soft', label: 'Alert only' },
                { value: 'hard', label: 'Hard block · requests refused with 402' },
              ]}
            />
          </Field>
        </div>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}

const bindingSchema = z.object({
  role: z.string().min(1, 'Pick a role'),
  kind: z.enum(['gateway', 'organization', 'team', 'project']),
  target: z.string(),
});

/** Adds a role binding for a user at a scope. */
export function BindingDialog({ user, onClose }: { user: string; onClose: () => void }) {
  const roles = useDirectory((s) => s.roles.data) ?? [];
  const teams = useTeams((s) => s.items);
  const projects = useProjects((s) => s.items);
  const orgs = useOrganizations((s) => s.items);
  const addBinding = useDirectory((s) => s.addBinding);
  const [values, setValues] = useState({
    role: '',
    kind: 'team' as 'gateway' | 'organization' | 'team' | 'project',
    target: teams[0]?.name ?? '',
  });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const role = roles.find((r) => r.name === values.role);
  const kinds = (role?.bindable_at ?? ['gateway', 'organization', 'team', 'project']).map((k) => ({
    value: k,
    label: k,
  }));
  const targets =
    values.kind === 'organization'
      ? orgs.map((o) => o.name)
      : values.kind === 'team'
        ? teams.map((t) => t.name)
        : values.kind === 'project'
          ? projects.map((p) => p.name)
          : [];

  async function submit() {
    const result = validate(bindingSchema, values);
    if (!result.ok || (values.kind !== 'gateway' && !values.target)) {
      setError(new Error('Pick a role and a scope.'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await addBinding({
        principal: { type: 'user', id: user },
        role: values.role,
        scope: values.kind === 'gateway' ? 'gateway' : `${values.kind}/${values.target}`,
      });
      notify('success', 'Role assigned', `${roleLabel(values.role)} to ${user}`);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      dirty={edited}
      open
      onClose={onClose}
      title={`Assign a role to ${user}`}
      size="sm"
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Assign role
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        <Field
          label="Role"
          hint={role ? (ROLE_DESCRIPTIONS[role.name] ?? `${role.permissions.length} permissions`) : undefined}
        >
          <Select
            value={values.role}
            onChange={(v) => setValues({ ...values, role: v })}
            placeholder="Select a role"
            options={roles.map((r) => ({ value: r.name, label: roleLabel(r.name) }))}
          />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Scope">
            <Select
              value={values.kind}
              onChange={(v) => setValues({ ...values, kind: v as typeof values.kind, target: '' })}
              options={kinds}
            />
          </Field>
          {values.kind === 'gateway' ? null : (
            <Field label={values.kind}>
              <Select
                value={values.target}
                onChange={(v) => setValues({ ...values, target: v })}
                placeholder="Select one"
                options={targets.map((t) => ({ value: t, label: t }))}
              />
            </Field>
          )}
        </div>
        <p className="text-caption text-muted">
          Admin access is always a role binding; API key scopes never grant it.
        </p>
      </div>
    </Dialog>
  );
}

const inviteSchema = z.object({
  email,
  displayName: z.string().trim().max(200),
  team: z.string(),
  role: z.string(),
});

/** Invites a person by email with a team and role; the gateway emails the link. */
export function InviteDialog({ onClose, onLink }: { onClose: () => void; onLink: (link: string) => void }) {
  const teams = useTeams((s) => s.items);
  const roles = useDirectory((s) => s.roles.data) ?? [];
  const invite = useDirectory((s) => s.invite);
  const [values, setValues] = useState({
    email: '',
    displayName: '',
    team: teams[0]?.name ?? '',
    role: 'developer',
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  async function submit() {
    const result = validate(inviteSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      const link = await invite({
        email: values.email.trim(),
        ...(values.displayName ? { display_name: values.displayName } : {}),
        role_bindings:
          values.team && values.role
            ? [
                {
                  principal: { type: 'user', id: values.email.trim() },
                  role: values.role,
                  scope: `team/${values.team}`,
                },
              ]
            : [],
      });
      notify('success', 'Invitation sent', values.email);
      if (link) onLink(link);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      dirty={edited}
      open
      onClose={onClose}
      title="Invite people"
      size="sm"
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Send invite
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
        <Field label="Email" error={errors.email} required>
          <Input
            type="email"
            value={values.email}
            onChange={(e) => setValues({ ...values, email: e.target.value })}
            data-autofocus
          />
        </Field>
        <Field label="Name" optional>
          <Input
            value={values.displayName}
            onChange={(e) => setValues({ ...values, displayName: e.target.value })}
          />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Team">
            <Combobox
              value={values.team}
              onChange={(v) => setValues({ ...values, team: v })}
              options={teams.map((t) => ({ value: t.name, label: t.name }))}
            />
          </Field>
          <Field label="Role">
            <Select
              value={values.role}
              onChange={(v) => setValues({ ...values, role: v })}
              options={roles.map((r) => ({ value: r.name, label: roleLabel(r.name) }))}
            />
          </Field>
        </div>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Dialog>
  );
}
