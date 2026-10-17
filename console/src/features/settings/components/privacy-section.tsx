'use client';

import { useEffect, useState } from 'react';
import { z } from 'zod';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, RadioGroup, Select, Switch, Textarea } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { MenuButton } from '@/components/ui/menu';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { parseDuration, plural } from '@/lib/format';
import { duration, validate, type FieldErrors } from '@/lib/validation';
import { scopeOrganization, useScope } from '@/stores/scope';
import { notify } from '@/stores/ui';

import { type GovernanceSettings, type GuardrailResource, settingsApi } from '../api';
import { type PolicyLevel, usePrivacy, type ZdrLevel } from '../privacy-store';
import { useOrgSettings } from '../store';
import { Card, Facts, SaveBar } from './parts';

type OnPii = 'mask' | 'hash' | 'drop_content';

interface ContentValues {
  content: boolean;
  zdr: boolean;
  redact: boolean;
  onPii: OnPii;
}

const PII_FOUND = 'Email, phone, credit card, US Social Security number, IBAN, IP address, Canadian SIN';
const SECRETS_FOUND =
  'API keys of OpenAI, Anthropic, AWS, GitHub, Slack, Google and Stripe; tokens (JWT); private keys';

const LEVEL_LABEL: Record<ZdrLevel['kind'], string> = {
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
  key: 'Key',
};

function ZdrElsewhere({ levels }: { levels: ZdrLevel[] }) {
  if (levels.length === 0) return null;
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-caption text-muted">
      <span>Also on for</span>
      {levels.slice(0, 12).map((l) => (
        <Badge key={`${l.kind}/${l.name}`}>
          {LEVEL_LABEL[l.kind]} <span className="mono">{l.name}</span>
        </Badge>
      ))}
      {levels.length > 12 ? <span>and {levels.length - 12} more</span> : null}
    </div>
  );
}

function ContentForm({
  level,
  initial,
  zdr,
}: {
  level: PolicyLevel;
  initial: ContentValues;
  zdr: ZdrLevel[];
}) {
  const savePolicy = usePrivacy((s) => s.savePolicy);
  const canEdit = useCan(level.organization ? 'patchOrganization' : 'patchPoliciesSettings');
  const [values, setValues] = useState(initial);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const dirty = JSON.stringify(values) !== JSON.stringify(initial);
  const set = (patch: Partial<ContentValues>) => setValues({ ...values, ...patch });
  const locked = !canEdit;
  const reason = "Your role can't change these settings";

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      await savePolicy({
        observability: {
          zero_data_retention: values.zdr,
          content: { enabled: values.content, redact_pii: values.redact, on_pii: values.onPii },
        },
      });
      notify('success', 'Data settings saved', level.organization ?? undefined);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      className="flex flex-col gap-5"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      <div className="flex flex-col gap-1">
        <Switch
          label="Keep prompts and responses"
          checked={values.content && !values.zdr}
          disabled={locked || values.zdr}
          disabledReason={values.zdr ? 'Zero data retention is on, so nothing is kept' : reason}
          onChange={(content) => set({ content })}
        />
        <p className="text-caption text-muted">
          Off: the request log keeps only details such as model, tokens, cost and timing.
        </p>
      </div>
      <div className="flex flex-col gap-3 rounded-control border border-line px-3.5 py-3">
        <div className="flex flex-col gap-1">
          <Switch
            label="Remove personal data before storing"
            checked={values.redact}
            disabled={locked || values.zdr || !values.content}
            disabledReason={locked ? reason : 'Applies only while prompts and responses are kept'}
            onChange={(redact) => set({ redact })}
          />
          <p className="text-caption text-muted">
            Stored and exported copies are checked first. Apps and providers still see the original text.
          </p>
        </div>
        {values.redact && values.content && !values.zdr ? (
          <RadioGroup<OnPii>
            label="When personal data or a secret is found"
            value={values.onPii}
            onChange={(onPii) => set({ onPii })}
            options={[
              {
                value: 'mask',
                label: 'Replace it with its type',
                description: 'For example "Write to [EMAIL]".',
              },
              {
                value: 'hash',
                label: 'Replace it with a hash',
                description: 'The same value always gives the same hash, so you can still group requests.',
              },
              {
                value: 'drop_content',
                label: "Don't store the request's content",
                description: 'Only its details are kept.',
              },
            ]}
          />
        ) : null}
        <Facts
          items={[
            { label: 'Personal data it finds', value: PII_FOUND },
            { label: 'Secrets it finds', value: SECRETS_FOUND },
          ]}
        />
      </div>
      <div className="flex flex-col gap-1">
        <Switch
          label="Zero data retention"
          checked={values.zdr}
          disabled={locked}
          disabledReason={reason}
          onChange={(zdr) => set({ zdr })}
        />
        <p className="text-caption text-muted">
          No prompt, response or provider error text is stored, exported or sent in alerts. Usage, cost and
          timing are still kept, and apps can&apos;t ask for content to be kept.
        </p>
        <ZdrElsewhere levels={zdr} />
      </div>
      <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
    </form>
  );
}

/** Content logging, zero data retention and stored-copy redaction of the scope's policy level. */
function ContentCard() {
  const policy = usePrivacy((s) => s.policy);
  const zdr = usePrivacy((s) => s.zdr);
  const level = policy.data;
  const content = level?.observability.content;
  return (
    <Card
      title="Prompts and responses"
      description={
        level?.organization
          ? `For ${level.organization}'s traffic, unless a team, project or key sets its own.`
          : 'For all traffic, unless an organization, team, project or key sets its own.'
      }
    >
      {policy.error ? <ErrorState error={policy.error} subject="Data settings" size="sm" /> : null}
      {policy.status === 'loading' && !level ? <LoadingRows rows={4} /> : null}
      {level ? (
        <ContentForm
          key={`${level.organization ?? ''}-${level.etag ?? ''}`}
          level={level}
          zdr={zdr.data ?? []}
          initial={{
            content: content?.enabled === true,
            zdr: level.observability.zero_data_retention === true,
            redact: content?.redact_pii === true,
            onPii: content?.on_pii ?? 'mask',
          }}
        />
      ) : null}
    </Card>
  );
}

/** A protected path's threshold in seconds, and how it was written ("90d"), if any. */
function protectedLimit(
  governance: GovernanceSettings | null,
  path: string,
): { seconds: number; text: string } | 'always' | null {
  const cc = governance?.spec.change_control;
  if (!cc) return null;
  if (cc.resources?.includes('settings')) return 'always';
  const entry = cc.protected_paths?.find((p) => p.path === path);
  if (!entry) return null;
  if (!entry.above) return 'always';
  const seconds = parseDuration(entry.above);
  return seconds === null ? 'always' : { seconds, text: entry.above };
}

function needsApproval(limit: ReturnType<typeof protectedLimit>, before: string, after: string): boolean {
  if (limit === null || before === after) return false;
  if (limit === 'always') return true;
  const s = parseDuration(after);
  return s !== null && s > limit.seconds;
}

function limitHint(limit: ReturnType<typeof protectedLimit>, fallback: string): string {
  if (limit === null) return fallback;
  if (limit === 'always') return 'Changes need a second person’s approval.';
  return `Longer than ${limit.text} needs a second person’s approval.`;
}

const RETENTION_PATH = 'observability.request_logs.sinks.store';

const retentionSchema = z
  .object({
    retention: duration.refine((v) => (parseDuration(v) ?? 0) >= 3600, 'Keep them for at least 1h'),
    contentRetention: duration.refine((v) => (parseDuration(v) ?? 0) >= 3600, 'Keep them for at least 1h'),
  })
  .refine((v) => (parseDuration(v.contentRetention) ?? 0) <= (parseDuration(v.retention) ?? 0), {
    path: ['contentRetention'],
    message: 'Keep prompts and responses no longer than the request details',
  });

function RetentionForm({
  initial,
  governance,
}: {
  initial: { retention: string; contentRetention: string };
  governance: GovernanceSettings | null;
}) {
  const saveObservability = useOrgSettings((s) => s.saveObservability);
  const canEdit = useCan('patchObservabilitySettings');
  const [values, setValues] = useState(initial);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const dirty = JSON.stringify(values) !== JSON.stringify(initial);
  const metaLimit = protectedLimit(governance, `${RETENTION_PATH}.retention`);
  const contentLimit = protectedLimit(governance, `${RETENTION_PATH}.content_retention`);
  const approval =
    needsApproval(metaLimit, initial.retention, values.retention) ||
    needsApproval(contentLimit, initial.contentRetention, values.contentRetention);

  async function submit() {
    const result = validate(retentionSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      await saveObservability({
        request_logs: {
          sinks: { store: { retention: values.retention, content_retention: values.contentRetention } },
        },
      });
      notify('success', 'Retention saved');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
        <Field
          label="Keep request details for"
          error={errors.retention}
          hint={limitHint(metaLimit, 'For example 30d or 90d.')}
        >
          <Input
            mono
            value={values.retention}
            disabled={!canEdit}
            onChange={(e) => setValues({ ...values, retention: e.target.value })}
          />
        </Field>
        <Field
          label="Keep prompts and responses for"
          error={errors.contentRetention}
          hint={limitHint(contentLimit, 'Applies only while they are kept.')}
        >
          <Input
            mono
            value={values.contentRetention}
            disabled={!canEdit}
            onChange={(e) => setValues({ ...values, contentRetention: e.target.value })}
          />
        </Field>
      </div>
      {approval ? (
        <p className="flex items-start gap-2 text-ui text-info" role="status">
          <Icon name="pullRequest" size={16} className="mt-0.5 shrink-0" />
          Saving opens a change request: this retention needs a second person&apos;s approval before it
          applies.
        </p>
      ) : null}
      <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
    </form>
  );
}

/** How long the request log keeps details and content, gateway-wide. */
function RetentionCard() {
  const observability = useOrgSettings((s) => s.observability);
  const governance = usePrivacy((s) => s.governance);
  const scope = useScope((s) => s.scope);
  const store = observability.data?.spec.request_logs?.sinks?.store;
  return (
    <Card
      title="Retention"
      description={
        scope.kind === 'gateway'
          ? 'How long the request log keeps each request.'
          : 'How long the request log keeps each request. Set once for the whole gateway.'
      }
    >
      {observability.error ? <ErrorState error={observability.error} subject="Retention" size="sm" /> : null}
      {observability.status === 'loading' && !observability.data ? <LoadingRows rows={2} /> : null}
      {observability.data ? (
        <RetentionForm
          key={`${observability.data.metadata.etag}-${governance.data?.metadata.etag ?? ''}`}
          governance={governance.data}
          initial={{
            retention: store?.retention ?? '30d',
            contentRetention: store?.content_retention ?? '7d',
          }}
        />
      ) : null}
    </Card>
  );
}

type Action = 'redact' | 'block' | 'flag';

const ACTION_LABEL: Record<string, string> = {
  redact: 'Replaced with its type',
  block: 'Request refused',
  flag: 'Flagged only',
};

const entitySchema = z.object({
  label: z
    .string()
    .trim()
    .regex(/^[A-Z][A-Z0-9_]{0,62}$/, 'Use capital letters, digits and _, starting with a letter'),
  patterns: z
    .string()
    .refine((v) => v.split('\n').some((l) => l.trim() !== ''), 'Enter at least one pattern')
    .refine((v) => v.split('\n').every((l) => l.length <= 1000), 'Keep each pattern under 1,000 characters')
    .refine((v) => v.split('\n').filter((l) => l.trim()).length <= 256, 'Use at most 256 patterns'),
});

/** The guardrail name a custom entity gets: its label in lower case. */
export function entityName(label: string): string {
  return `entity_${label.trim().toLowerCase()}`.slice(0, 63);
}

function EntityDrawer({
  open,
  onClose,
  level,
}: {
  open: boolean;
  onClose: () => void;
  level: PolicyLevel | null;
}) {
  const savePolicy = usePrivacy((s) => s.savePolicy);
  const loadGuardrails = usePrivacy((s) => s.loadGuardrails);
  const [label, setLabel] = useState('');
  const [patterns, setPatterns] = useState('');
  const [check, setCheck] = useState<'none' | 'luhn' | 'iban_mod97'>('none');
  const [action, setAction] = useState<Action>('redact');
  const [attach, setAttach] = useState(true);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  function close() {
    setLabel('');
    setPatterns('');
    setCheck('none');
    setAction('redact');
    setAttach(true);
    setErrors({});
    setError(null);
    onClose();
  }

  async function submit() {
    const result = validate(entitySchema, { label, patterns });
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const name = entityName(label);
    try {
      await settingsApi.createGuardrail(name, {
        type: 'regex',
        action,
        regex: {
          deny: patterns
            .split('\n')
            .map((l) => l.trim())
            .filter(Boolean),
          label: label.trim(),
          validate: check,
        },
      });
      if (attach && level) {
        const input = level.guardrails.input ?? [];
        if (!input.includes(name)) await savePolicy({ guardrails: { input: [...input, name] } });
      }
      await loadGuardrails();
      notify('success', 'Custom entity added', label.trim());
      close();
    } catch (e) {
      setError(e);
      void loadGuardrails();
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      open={open}
      onClose={close}
      title="Add custom entity"
      subtitle="Your own kind of sensitive value, such as a customer or account number."
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Add entity
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
          label="Entity type"
          error={errors.label}
          hint={
            label.trim()
              ? `Shown as [${label.trim()}] where a match is replaced.`
              : 'For example CUSTOMER_ID.'
          }
        >
          <Input
            mono
            data-autofocus
            value={label}
            placeholder="CUSTOMER_ID"
            onChange={(e) => setLabel(e.target.value.toUpperCase())}
          />
        </Field>
        <Field
          label="Patterns"
          error={errors.patterns}
          hint="One regular expression per line, such as CUST-[0-9]{6}. Add (?i) to ignore case."
        >
          <Textarea mono rows={4} value={patterns} onChange={(e) => setPatterns(e.target.value)} />
        </Field>
        <Field label="Extra check" hint="Only count a match that passes this check.">
          <Select
            value={check}
            onChange={(v) => setCheck(v as typeof check)}
            options={[
              { value: 'none', label: 'None' },
              { value: 'luhn', label: 'Luhn checksum (card-like numbers)' },
              { value: 'iban_mod97', label: 'IBAN checksum' },
            ]}
          />
        </Field>
        <RadioGroup<Action>
          label="When a request contains one"
          value={action}
          onChange={setAction}
          options={[
            {
              value: 'redact',
              label: 'Replace it with its type',
              description: 'The provider never sees it.',
            },
            { value: 'block', label: 'Refuse the request' },
            { value: 'flag', label: 'Let it through and flag the request' },
          ]}
        />
        <Switch
          label={level?.organization ? `Check every request of ${level.organization}` : 'Check every request'}
          checked={attach}
          onChange={setAttach}
        />
      </form>
    </Drawer>
  );
}

/** Custom entities: regex guardrails with a label, checked on the level's requests. */
function EntitiesCard() {
  const guardrails = usePrivacy((s) => s.guardrails);
  const policy = usePrivacy((s) => s.policy);
  const savePolicy = usePrivacy((s) => s.savePolicy);
  const loadGuardrails = usePrivacy((s) => s.loadGuardrails);
  const canDelete = useCan('deleteGuardrail');
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<GuardrailResource | null>(null);
  const level = policy.data;
  const input = level?.guardrails.input ?? [];
  const entities = (guardrails.data ?? []).filter((g) => g.spec.type === 'regex');
  return (
    <Card
      title="Custom entities"
      description="Your own kinds of sensitive values, found in requests by pattern and replaced or refused."
      actions={
        <Gate operation="createGuardrail" mode="hide">
          <Button size="sm" icon="plus" onClick={() => setAdding(true)}>
            Add entity
          </Button>
        </Gate>
      }
    >
      {guardrails.error ? <ErrorState error={guardrails.error} subject="Custom entities" size="sm" /> : null}
      {guardrails.status === 'loading' && !guardrails.data ? <LoadingRows rows={2} /> : null}
      {guardrails.data && entities.length === 0 ? (
        <EmptyState
          size="sm"
          title="No custom entities yet"
          description="Add one for values only your business has, such as customer IDs."
        />
      ) : null}
      {entities.length > 0 ? (
        <Table label="Custom entities">
          <thead>
            <tr>
              <Th>Entity</Th>
              <Th>Patterns</Th>
              <Th>When found</Th>
              <Th>Checked</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {entities.map((g) => {
              const deny = g.spec.regex?.deny ?? [];
              const on = input.includes(g.name);
              return (
                <Tr key={g.name}>
                  <Td>
                    <span className="mono text-fg">{g.spec.regex?.label ?? 'REDACTED'}</span>
                    <div className="mono text-caption text-dim">{g.name}</div>
                  </Td>
                  <Td>
                    <span className="mono text-caption text-fg">{deny[0] ?? '—'}</span>
                    {deny.length > 1 ? (
                      <span className="text-caption text-muted"> and {plural(deny.length - 1, 'more')}</span>
                    ) : null}
                  </Td>
                  <Td muted>{ACTION_LABEL[g.spec.action ?? 'block'] ?? g.spec.action}</Td>
                  <Td>
                    {g.spec.enabled === false ? (
                      <Badge>Turned off</Badge>
                    ) : on ? (
                      <Badge tone="success" dot>
                        {level?.organization ? `Every request of ${level.organization}` : 'Every request'}
                      </Badge>
                    ) : (
                      <span className="text-caption text-muted">Where a policy names it</span>
                    )}
                  </Td>
                  <Td className="text-right">
                    {canDelete ? (
                      <MenuButton
                        iconOnly
                        variant="ghost"
                        size="sm"
                        label={`Actions for ${g.name}`}
                        items={[
                          {
                            label: 'Remove entity',
                            icon: 'trash',
                            danger: true,
                            onSelect: () => setRemoving(g),
                          },
                        ]}
                      />
                    ) : null}
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
      <EntityDrawer open={adding} onClose={() => setAdding(false)} level={level} />
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove custom entity ${removing?.spec.regex?.label ?? ''}?`}
        objectName={removing?.name}
        confirmLabel="Remove entity"
        effect="Requests are no longer checked for it. Values already replaced in stored requests stay replaced."
        onConfirm={async (reason) => {
          if (!removing) return;
          if (input.includes(removing.name))
            await savePolicy({ guardrails: { input: input.filter((n) => n !== removing.name) } });
          await settingsApi.deleteGuardrail(removing.name, reason);
          await loadGuardrails();
          notify('success', 'Custom entity removed', removing.spec.regex?.label);
        }}
      />
    </Card>
  );
}

/** Data & privacy: what the request log keeps, zero data retention, redaction, retention, entities. */
export function PrivacySection() {
  const scope = useScope((s) => s.scope);
  const org = scopeOrganization(scope);
  const load = usePrivacy((s) => s.load);
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  useEffect(() => {
    void load(org);
  }, [org, load]);
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);
  return (
    <>
      <ContentCard />
      <RetentionCard />
      <EntitiesCard />
    </>
  );
}
