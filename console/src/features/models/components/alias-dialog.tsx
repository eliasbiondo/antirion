'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input } from '@/components/ui/form';
import { CancelButton, Dialog } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { modelOption } from '@/components/ui/marks';
import type { ModelSpec } from '@/lib/api/types';
import { useTeamOptions } from '@/lib/team-options';
import { useEdited } from '@/lib/hooks';
import { serverFieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { saveModelSpec, useModels } from '../store';

type Aliases = NonNullable<ModelSpec['aliases']>;

/** Why an alias name can't be used, or null: the gateway's rules, checked before the call. */
export function aliasNameError(
  name: string,
  taken: { models: string[]; aliases: string[] },
  previous?: string,
): string | null {
  const n = name.trim();
  if (!n) return 'Enter the name apps will send, e.g. chat-default';
  if (n.length > 128) return 'Use at most 128 characters';
  if (n.includes('*')) return 'An alias can’t contain *';
  if (!/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(n))
    return 'Use letters, digits and . _ : / - only, starting with a letter or digit';
  if (taken.models.includes(n)) return 'A model already has this name';
  if (n !== previous && taken.aliases.includes(n)) return 'Another alias already has this name';
  return null;
}

/** The spec with one alias added, renamed or changed, keeping its parameters. */
export function withAlias(
  spec: ModelSpec,
  previous: string | null,
  name: string,
  fields: { description: string; owner: string },
): ModelSpec {
  const aliases: Aliases = Object.fromEntries(
    Object.entries(spec.aliases ?? {}).filter(([k]) => k !== previous),
  );
  const kept = previous ? spec.aliases?.[previous] : undefined;
  const { description: _d, owner: _o, ...rest } = kept ?? {};
  aliases[name.trim()] = {
    ...rest,
    ...(fields.description.trim() ? { description: fields.description.trim() } : {}),
    ...(fields.owner ? { owner: fields.owner } : {}),
  };
  return { ...spec, aliases };
}

/** The spec without one alias. */
export function withoutAlias(spec: ModelSpec, name: string): ModelSpec {
  const aliases: Aliases = Object.fromEntries(Object.entries(spec.aliases ?? {}).filter(([k]) => k !== name));
  const { aliases: _a, ...rest } = spec;
  return Object.keys(aliases).length > 0 ? { ...rest, aliases } : rest;
}

/**
 * Adds or edits an alias: another name apps can send for a Model, with a description and the team
 * that owns it. Its parameters, if it has any, are kept.
 */
export function AliasDialog({
  model,
  alias,
  onClose,
}: {
  /** The Model the alias belongs to; null to choose one (adding from the aliases list). */
  model: string | null;
  /** The alias being edited, or null for a new one. */
  alias: string | null;
  onClose: () => void;
}) {
  const models = useModels((s) => s.items);
  const teams = useTeamOptions();
  const [target, setTarget] = useState(model ?? '');
  const current = models.find((m) => m.name === target);
  const existing = alias ? current?.spec.aliases?.[alias] : undefined;
  const [name, setName] = useState(alias ?? '');
  const [description, setDescription] = useState(existing?.description ?? '');
  const [owner, setOwner] = useState(existing?.owner ?? '');
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited({ target, name, description, owner });
  const taken = {
    models: models.map((m) => m.name),
    aliases: models.flatMap((m) => Object.keys(m.spec.aliases ?? {})),
  };

  async function save() {
    const nameError = aliasNameError(name, taken, alias ?? undefined);
    const next: Record<string, string> = {};
    if (nameError) next.name = nameError;
    if (!current) next.model = 'Select the model the alias names';
    setErrors(next);
    if (Object.keys(next).length > 0 || !current) return;
    setBusy(true);
    setError(null);
    try {
      await saveModelSpec(current.name, withAlias(current.spec, alias, name, { description, owner }));
      notify('success', alias ? 'Alias saved' : 'Alias added', `@${name.trim()} → ${current.name}`);
      onClose();
    } catch (e) {
      setError(e);
      const fields = serverFieldErrors(e, `models.${current.name}.aliases`);
      if (Object.keys(fields).length > 0) setErrors({ name: Object.values(fields)[0] ?? '' });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      dirty={edited}
      open
      onClose={onClose}
      title={alias ? `Edit @${alias}` : 'Add alias'}
      description="Another name apps can send for a model. Requests to it behave exactly like the model."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void save()}>
            {alias ? 'Save alias' : 'Add alias'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <InlineError error={error} />
        <Field label="Alias" error={errors.name} hint="Apps send this name in the model field.">
          <Input mono value={name} onChange={(e) => setName(e.target.value)} data-autofocus />
        </Field>
        {model === null ? (
          <Field label="Model" error={errors.model}>
            <Combobox
              value={target}
              onChange={setTarget}
              emptyOption="Select a model"
              options={models.map((m) => modelOption(m))}
            />
          </Field>
        ) : (
          <p className="text-ui text-muted">
            Model <span className="mono text-fg">{model}</span>
          </p>
        )}
        <Field label="Description" optional>
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <Field label="Owner" optional hint="The team that answers for this alias.">
          <Combobox value={owner} onChange={setOwner} emptyOption="No owner" options={teams} />
        </Field>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Dialog>
  );
}

/** Removes an alias after a confirmation that says what apps sending it will get. */
export function RemoveAliasDialog({
  model,
  alias,
  onClose,
}: {
  model: string;
  alias: string;
  onClose: () => void;
}) {
  return (
    <ConfirmDialog
      open
      onClose={onClose}
      title={`Remove @${alias}?`}
      objectName={`@${alias}`}
      confirmLabel="Remove alias"
      effect={`Apps sending @${alias} get an error from then on. ${model} keeps working under its own name.`}
      onConfirm={async (reason) => {
        const current = useModels.getState().byName(model);
        if (!current) return;
        await saveModelSpec(model, withoutAlias(current.spec, alias), reason);
        notify('success', 'Alias removed', `@${alias}`);
      }}
    />
  );
}
