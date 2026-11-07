'use client';

import { type ReactNode, useState } from 'react';

import { Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog, useSwitchChanges } from '@/components/ui/confirm';
import { Segmented, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { DefinitionGrid, Panel } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { EmptyAction, EmptyState, InlineError } from '@/components/ui/states';
import type { GuardrailResource } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount } from '@/lib/format';
import { notify } from '@/stores/ui';

import {
  applyStrictness,
  type CustomKind,
  customKindErrors,
  type CustomKindErrors,
  customKindsOf,
  customMethod,
  detectionMethod,
  ENTITY_NAME,
  kindEntity,
  kindInText,
  kindsOf,
  maskedSample,
  patternPlaceholder,
  type PiiPreserve,
  PII_ENTITIES,
  SECRET_DETECTORS,
  type Strictness,
  STRICTNESS,
  strictnessChanges,
  strictnessOf,
} from '../rules';
import { defaultPiiSpec, useSafety } from '../store';
import { blankKind, CustomKindFields, type EditorTarget, useAttachment } from './rule-editor';
import { translateNow } from '@/lib/i18n';

type Rule = GuardrailResource;

const on = (g: Rule) => g.spec.enabled !== false;

/**
 * The personal data rule the tab starts on: the first one on that runs on every prompt, else the
 * first one on, else the first.
 */
function startingRule(rules: Rule[], input: string[]): Rule | null {
  return rules.find((g) => on(g) && input.includes(g.name)) ?? rules.find(on) ?? rules[0] ?? null;
}

const WHEN_FOUND: Record<string, string> = {
  label: 'Replaced with a label',
  mask: 'Masked',
  hash: 'Replaced with a hash',
};

/** What a rule does with a value it finds, in words. */
function whenFound(rule: Rule): string {
  const action = rule.spec.action ?? 'redact';
  if (action === 'block') return 'Request refused';
  if (action === 'flag') return 'Flagged only';
  if (action === 'divert') return 'Another Model answers';
  return WHEN_FOUND[rule.spec.replacement ?? 'label'] ?? 'Replaced';
}

/**
 * The chosen rule in a few facts: on or off, where it runs, what it does and, for a rule that can
 * put values back (`values`), whether it does.
 */
function RuleFacts({ rule, values = false }: { rule: Rule; values?: boolean }) {
  const attached = useAttachment(rule.name);
  const runs =
    attached.input && attached.output
      ? 'Every prompt and answer'
      : attached.input
        ? 'Every prompt'
        : attached.output
          ? 'Every answer'
          : 'Where a policy names it';
  const redacts = (rule.spec.action ?? 'redact') === 'redact';
  return (
    <DefinitionGrid
      columns={values ? 4 : 3}
      items={[
        { label: 'Status', value: on(rule) ? 'On' : 'Turned off' },
        { label: 'Checks', value: runs },
        { label: 'When found', value: whenFound(rule) },
        ...(values
          ? [
              {
                label: 'Values in the answer',
                value: !redacts ? '—' : rule.spec.pii?.reversible ? 'Put back' : 'Stay replaced',
              },
            ]
          : []),
      ]}
    />
  );
}

/** The presets over the chosen rule's built-in kinds, what stays readable and how values look. */
function Presets({ rule }: { rule: Rule }) {
  const updateRule = useSafety((s) => s.updateRule);
  const canWrite = useCan('replaceGuardrail');
  const [pending, setPending] = useState<Strictness | null>(null);
  const current = strictnessOf(rule.spec);
  const changes = pending ? strictnessChanges(rule.spec, pending) : [];
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-3">
        <Segmented<string>
          label="Preset"
          value={current ?? 'custom'}
          onChange={(v) => {
            if (canWrite && (v === 'loose' || v === 'balanced' || v === 'strict')) setPending(v);
          }}
          options={[
            ...(['loose', 'balanced', 'strict'] as const).map((k) => ({
              value: k,
              label: STRICTNESS[k].label,
            })),
            ...(current ? [] : [{ value: 'custom', label: 'Custom' }]),
          ]}
        />
        <span className="text-caption text-muted">
          {current ? STRICTNESS[current].summary : 'Its settings match no preset.'}
        </span>
      </div>
      <p className="text-caption text-muted">
        A preset sets which built-in kinds <span className="mono text-fg">{rule.name}</span> looks for, what
        stays readable and how values are replaced. Kinds of your own stay as they are.
      </p>
      <ConfirmDialog
        open={pending !== null}
        onClose={() => setPending(null)}
        tone="primary"
        withReason={false}
        title={pending ? `Make ${rule.name} ${STRICTNESS[pending].label.toLowerCase()}?` : ''}
        objectName={rule.name}
        effect={
          changes.length === 0 ? (
            'Nothing changes: the rule already matches this preset.'
          ) : (
            <ul className="flex list-disc flex-col gap-1 pl-5">
              {changes.map((c) => (
                <li key={c}>{c}.</li>
              ))}
            </ul>
          )
        }
        confirmLabel={pending ? `Make it ${STRICTNESS[pending].label.toLowerCase()}` : 'Apply'}
        onConfirm={async () => {
          if (!pending) return;
          await updateRule(rule, applyStrictness(rule.spec, pending));
          notify('success', translateNow('{0} is {1}', rule.name, STRICTNESS[pending].label.toLowerCase()));
        }}
      />
    </div>
  );
}

/**
 * No rule of a family yet: one step creates the rule it starts from and runs it on every prompt and
 * answer; "Set it up yourself" opens the rule editor on its method.
 */
function NoRule({ family, onEdit }: { family: Family; onEdit: (t: EditorTarget) => void }) {
  const create = useSafety((s) => (family.method === 'secrets' ? s.createSecretsRule : s.createPiiRule));
  const policies = useSafety((s) => s.policies.data);
  const canCreate = useCan('createGuardrail');
  const canAttach = useCan('patchPoliciesSettings') && policies !== null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  return (
    <>
      <EmptyState
        size="sm"
        title={`No rule checks ${family.noun}`}
        description={family.offer(canAttach)}
        action={
          canCreate ? (
            <EmptyAction
              disabled={busy}
              onClick={() => {
                setBusy(true);
                setError(null);
                create(canAttach)
                  .then((name) => notify('success', family.done(canAttach), name))
                  .catch((e: unknown) => setError(e))
                  .finally(() => setBusy(false));
              }}
            >
              {`Turn on ${family.short} checks`}
            </EmptyAction>
          ) : undefined
        }
        secondaryAction={
          canCreate ? (
            <EmptyAction onClick={() => onEdit({ kind: 'new', method: family.method })}>
              Set it up yourself
            </EmptyAction>
          ) : undefined
        }
      />
      <InlineError error={error} />
    </>
  );
}

/** Adds a kind of personal data to the chosen rule. */
function AddKindDrawer({ rule, open, onClose }: { rule: Rule; open: boolean; onClose: () => void }) {
  const addKind = useSafety((s) => s.addKind);
  const [form, setForm] = useState<CustomKind>(blankKind());
  const [errors, setErrors] = useState<CustomKindErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const close = () => {
    setForm(blankKind());
    setErrors({});
    setError(null);
    onClose();
  };
  async function submit() {
    const kind = { ...form, name: form.name.trim(), label: form.label.trim() };
    const found = customKindErrors(kind, customKindsOf(rule.spec));
    if (Object.keys(found).length > 0) {
      setErrors(found);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await addKind(rule.name, kind);
      notify('success', translateNow('{0} looks for {1}', rule.name, kind.name));
      close();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Drawer
      open={open}
      onClose={close}
      dirty={form.name !== '' || form.label !== '' || form.pattern !== ''}
      title="Add a kind of personal data"
      subtitle={
        <>
          Looked for by <span className="mono">{rule.name}</span> beside the built-in kinds.
        </>
      }
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Add kind
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
        <CustomKindFields
          value={form}
          onChange={(f) => {
            setForm(f);
            setErrors({});
          }}
          errors={errors}
          numbered={rule.spec.pii?.reversible === true}
        />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}

interface KindCard {
  /** The kind's id: a built-in's, or a rule's own name. */
  id: string;
  name: string;
  /** The entity events and reports name it with. */
  entity: string;
  /** The definition, for a kind of a rule's own. */
  own: CustomKind | null;
  method: string;
}

/**
 * Every kind of personal data a card shows: the built-ins, then each rule's own, once each (the
 * chosen rule's definition first).
 */
function kindCards(rules: Rule[], chosen: Rule | null): KindCard[] {
  const cards: KindCard[] = PII_ENTITIES.map((e) => ({
    id: e,
    name: ENTITY_NAME[e] ?? e,
    entity: e,
    own: null,
    method: detectionMethod(e),
  }));
  const ordered = chosen ? [chosen, ...rules.filter((r) => r !== chosen)] : rules;
  for (const r of ordered)
    for (const k of customKindsOf(r.spec))
      if (!cards.some((c) => c.id === k.name))
        cards.push({ id: k.name, name: k.name, entity: kindEntity(k), own: k, method: customMethod(k) });
  return cards;
}

/** The custom entities of Settings › Data & privacy: pattern rules named `entity_<label>`. */
function entityRules(guardrails: Rule[]): Rule[] {
  return guardrails.filter((g) => g.spec.type === 'regex' && g.name.startsWith('entity_'));
}

/** Who checks a kind, in words: the chosen rule first, then the others that are on. */
function checkedBy(id: string, chosen: Rule | null, rules: Rule[]): { text: string; checked: boolean } {
  const by = rules.filter((r) => on(r) && kindsOf(r.spec).includes(id)).map((r) => r.name);
  if (chosen && kindsOf(chosen.spec).includes(id) && !on(chosen))
    return {
      text: by.length > 0 ? `Checked by ${by.join(', ')}` : `${chosen.name} is turned off`,
      checked: by.length > 0,
    };
  if (by.length === 0) return { text: 'Not checked', checked: false };
  const first = chosen && by.includes(chosen.name) ? chosen.name : (by[0] ?? '');
  const others = by.length - 1;
  return { text: `Checked by ${first}${others > 0 ? ` and ${others} more` : ''}`, checked: true };
}

/** A card: what it is, its matches over 24 hours, how a value looks, who checks it, a switch. */
function Card({
  name,
  mono,
  matches,
  sample,
  status,
  method,
  onFilter,
  toggle,
}: {
  name: string;
  /** The name is an identifier (a kind of a rule's own, a pattern rule's label). */
  mono?: boolean;
  matches: string;
  sample: string;
  status: { text: string; checked: boolean };
  method: string;
  onFilter: () => void;
  toggle?: { checked: boolean; label: string; disabledReason?: string; onChange: (on: boolean) => void };
}) {
  return (
    <li className="relative">
      <button
        type="button"
        onClick={onFilter}
        className={cn(
          'flex h-full w-full flex-col gap-2 rounded-sm border border-line bg-raised px-3.5 py-3 text-left hover:border-line-strong hover:bg-hover',
          toggle && 'pr-14',
          !status.checked && 'opacity-70',
        )}
      >
        <span className={cn('text-ui font-medium text-fg', mono && 'mono')}>{name}</span>
        <span className="flex items-baseline gap-1.5">
          <span className="mono text-title font-semibold text-fg tabular-nums">{matches}</span>
          <span className="text-caption text-muted">matches, 24 h</span>
        </span>
        <span className={cn('mono truncate text-caption', status.checked ? 'text-fg' : 'text-muted')}>
          {sample}
        </span>
        <span className="flex items-center gap-1.5 text-caption text-muted">
          <Dot tone={status.checked ? 'success' : 'neutral'} />
          <span className="truncate">{status.text}</span>
        </span>
        <span className="text-caption text-dim">{method}</span>
      </button>
      {toggle ? (
        <span className="absolute top-3 right-3.5">
          <Switch
            hideLabel
            label={toggle.label}
            checked={toggle.checked}
            disabled={toggle.disabledReason !== undefined}
            disabledReason={toggle.disabledReason}
            onChange={toggle.onChange}
          />
        </span>
      ) : null}
    </li>
  );
}

const GRID = 'grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4';

/** A family of kinds a rule looks for, personal data or secrets, in the words its section uses. */
interface Family {
  /** The rule editor's method for it. */
  method: 'pii' | 'secrets';
  /** In running text: "personal data", "secrets". */
  noun: string;
  /** The rule panel's title, for one rule and for several. */
  title: [string, string];
  /** In "Turn on … checks". */
  short: string;
  /** What choosing one of several rules changes. */
  chosen: string;
  /** What the one step adds, in words; `attach` when it can also run the rule everywhere. */
  offer: (attach: boolean) => string;
  /** What the one step did. */
  done: (attach: boolean) => string;
}

const EVERYWHERE = ' on every prompt and answer';

const PERSONAL_DATA: Family = {
  method: 'pii',
  noun: 'personal data',
  title: ['Personal data rule', 'Personal data rules'],
  short: 'personal data',
  chosen: 'the preset and the cards change the one chosen',
  offer: (attach) =>
    `One step adds a rule that replaces ${defaultPiiSpec().pii?.entities?.length ?? 0} kinds, such as emails and card numbers, with labels${attach ? EVERYWHERE : ''}.`,
  done: (attach) => (attach ? `Personal data is checked${EVERYWHERE}` : 'Personal data rule created'),
};

const SECRETS: Family = {
  method: 'secrets',
  noun: 'secrets',
  title: ['Secrets rule', 'Secrets rules'],
  short: 'secret',
  chosen: 'the cards change the one chosen',
  offer: (attach) =>
    `One step adds a rule that replaces all ${SECRET_DETECTORS.length} kinds, such as API keys and private keys, with labels${attach ? EVERYWHERE : ''}. Replacing, not refusing, lets a request through without the key.`,
  done: (attach) => (attach ? `Secrets are checked${EVERYWHERE}` : 'Secrets rule created'),
};

/** How a value looks once the rule acts on it: its placeholder, or what happens instead. */
function sampleOf(rule: Rule | null, entity: string, preserve: PiiPreserve[], numbered: boolean): string {
  if (rule && (rule.spec.action ?? 'redact') !== 'redact') return whenFound(rule);
  return maskedSample(entity, rule?.spec.replacement ?? 'label', preserve, numbered);
}

/** "Add a kind" and its drawer, for the chosen personal data rule. */
function AddKind({ rule }: { rule: Rule }) {
  const [adding, setAdding] = useState(false);
  return (
    <>
      <Button size="sm" icon="plus" onClick={() => setAdding(true)}>
        Add a kind
      </Button>
      <AddKindDrawer rule={rule} open={adding} onClose={() => setAdding(false)} />
    </>
  );
}

/** A kind a card's switch looks for: its id, its name in running text, its definition if its own. */
interface SwitchKind {
  id: string;
  what: string;
  own?: CustomKind | null;
}

type CardToggle = NonNullable<Parameters<typeof Card>[0]['toggle']>;

/**
 * One family's section: which rule or rules look for it (a picker when there are several) with the
 * chosen rule's facts, or the one step that adds a rule; then a card per kind. A card's switch
 * looks for its kind with the chosen rule or stops, with Undo.
 */
function KindsSection({
  family,
  rules,
  onEdit,
  details,
  actions,
  cards,
}: {
  family: Family;
  rules: Rule[];
  onEdit: (t: EditorTarget) => void;
  /** Below the facts, for the chosen rule. */
  details?: (rule: Rule) => ReactNode;
  /** Beside the kinds' title, for the chosen rule. */
  actions?: (rule: Rule) => ReactNode;
  cards: (rule: Rule | null, toggle: (kind: SwitchKind) => CardToggle | undefined) => ReactNode;
}) {
  const guardrails = useSafety((s) => s.guardrails.data);
  const input = useSafety((s) => s.policies.data?.spec.guardrails?.input);
  const setKind = useSafety((s) => s.setKind);
  const addKind = useSafety((s) => s.addKind);
  const canWrite = useCan('replaceGuardrail');
  const switches = useSwitchChanges();
  const [chosenName, setChosenName] = useState<string | null>(null);
  const rule = rules.find((r) => r.name === chosenName) ?? startingRule(rules, input ?? []);

  const toggleFor = ({ id, what, own }: SwitchKind): CardToggle | undefined => {
    if (!rule || !canWrite) return undefined;
    const kinds = kindsOf(rule.spec);
    const looked = kinds.includes(id);
    const last = looked && kinds.length === 1;
    // A kind of a rule's own is its definition: turning it on adds it (from whichever rule has
    // it), turning it off removes it, so that asks first and Undo puts it back.
    const turn = (next: boolean) => (own && next ? addKind(rule.name, own) : setKind(rule.name, id, next));
    return {
      checked: looked,
      label: `Look for ${what} with ${rule.name}`,
      ...(last ? { disabledReason: 'A rule looks for at least one kind; turn the rule off instead' } : {}),
      onChange: (next: boolean) =>
        switches.change({
          name: rule.name,
          apply: () => turn(next),
          undo: () => turn(!next),
          done: next ? `${rule.name} looks for ${what}` : `${rule.name} stops looking for ${what}`,
          undone: next ? `${rule.name} stops looking for ${what}` : `${rule.name} looks for ${what}`,
          ask:
            own && !next
              ? {
                  title: `Stop looking for ${what} with ${rule.name}?`,
                  effect: `Its pattern is removed from ${rule.name}. Undo, or adding it again, puts it back.`,
                  confirmLabel: 'Stop looking for it',
                }
              : undefined,
        }),
    };
  };

  return (
    <>
      {switches.dialog}
      <Panel
        title={rules.length > 1 ? family.title[1] : family.title[0]}
        subtitle={
          !guardrails
            ? undefined
            : rules.length === 0
              ? `Nothing looks for ${family.noun} yet`
              : rules.length === 1
                ? `The rule that finds ${family.noun} in requests`
                : `${rules.length} rules find ${family.noun} · ${family.chosen}`
        }
        actions={
          rule ? (
            <>
              {rules.length > 1 ? (
                <div className="w-56">
                  <Select
                    aria-label="Rule"
                    size="sm"
                    mono
                    value={rule.name}
                    onChange={setChosenName}
                    options={rules.map((r) => ({
                      value: r.name,
                      label: r.name,
                      ...(on(r) ? {} : { description: 'Turned off' }),
                    }))}
                  />
                </div>
              ) : (
                <span className="mono text-ui text-fg">{rule.name}</span>
              )}
              <Button size="sm" onClick={() => onEdit({ kind: 'edit', name: rule.name })}>
                Edit rule
              </Button>
            </>
          ) : undefined
        }
      >
        {guardrails && !rule ? <NoRule family={family} onEdit={onEdit} /> : null}
        {rule ? (
          <div className="flex flex-col gap-4">
            <RuleFacts rule={rule} values={family.method === 'pii'} />
            {details?.(rule)}
          </div>
        ) : null}
      </Panel>
      <Panel
        title={`Kinds of ${family.noun}`}
        subtitle={
          rule
            ? `Matches in the last 24 hours · a switch changes what ${rule.name} looks for · choose a card to see its events`
            : 'Matches in the last 24 hours · choose a card to see its events'
        }
        actions={rule && canWrite && actions ? actions(rule) : undefined}
        bodyClassName="pb-5"
      >
        <ul className={GRID} aria-label={`Kinds of ${family.noun}`}>
          {cards(rule, toggleFor)}
        </ul>
      </Panel>
    </>
  );
}

/**
 * The Personal data tab: a section for personal data and one for secrets, each with which rule or
 * rules look for it, the chosen rule's facts (and, for personal data, its presets), then a card per
 * kind with its matches over 24 hours, how a value looks and who checks it. A card's switch looks
 * for its kind with the chosen rule or stops; choosing a card filters the events to it.
 */
export function PiiPanel({
  onFilter,
  onEdit,
}: {
  onFilter: (entity: string) => void;
  onEdit: (t: EditorTarget) => void;
}) {
  const byEntity = useSafety((s) => s.byEntity.data);
  const guardrails = useSafety((s) => s.guardrails.data);
  const all = guardrails ?? [];
  const piiRules = all.filter((g) => g.spec.type === 'pii');
  const secretsRules = all.filter((g) => g.spec.type === 'secrets');
  const count = (id: string, checked: boolean) =>
    byEntity && (checked || (byEntity[id] ?? 0) > 0) ? formatCount(byEntity[id] ?? 0) : '—';

  return (
    <div className="flex flex-col">
      <KindsSection
        family={PERSONAL_DATA}
        rules={piiRules}
        onEdit={onEdit}
        details={(rule) => <Presets rule={rule} />}
        actions={(rule) => <AddKind key={rule.name} rule={rule} />}
        cards={(rule, toggle) => {
          const preserve = rule?.spec.pii?.preserve ?? [];
          const numbered = rule?.spec.pii?.reversible === true;
          return (
            <>
              {kindCards(piiRules, rule).map((c) => {
                const status = checkedBy(c.id, rule, piiRules);
                return (
                  <Card
                    key={c.id}
                    name={c.name}
                    mono={c.own !== null}
                    matches={count(c.entity, status.checked)}
                    sample={sampleOf(rule, c.entity, c.own ? [] : preserve, numbered)}
                    status={status}
                    method={c.method}
                    onFilter={() => onFilter(c.entity)}
                    toggle={toggle({
                      id: c.id,
                      what: c.own ? c.name : kindInText(c.id),
                      own: c.own,
                    })}
                  />
                );
              })}
              {entityRules(all).map((g) => {
                const label = g.spec.regex?.label ?? '';
                const entity = (label || 'redacted').toLowerCase();
                const reversible = g.spec.regex?.reversible === true;
                const action = g.spec.action ?? 'block';
                return (
                  <Card
                    key={g.name}
                    name={label || g.name}
                    mono
                    matches={count(entity, on(g))}
                    sample={
                      action === 'redact'
                        ? patternPlaceholder(label, g.name, reversible)
                        : action === 'flag'
                          ? 'Flagged only'
                          : 'Request blocked'
                    }
                    status={{
                      text: on(g) ? `Checked by ${g.name}` : `${g.name} is turned off`,
                      checked: on(g),
                    }}
                    method="Pattern rule, from Settings › Data & privacy"
                    onFilter={() => onFilter(entity)}
                  />
                );
              })}
            </>
          );
        }}
      />
      <KindsSection
        family={SECRETS}
        rules={secretsRules}
        onEdit={onEdit}
        cards={(rule, toggle) =>
          SECRET_DETECTORS.map((d) => {
            const status = checkedBy(d, rule, secretsRules);
            return (
              <Card
                key={d}
                name={ENTITY_NAME[d] ?? d}
                matches={count(d, status.checked)}
                sample={sampleOf(rule, d, [], false)}
                status={status}
                method={detectionMethod(d, rule?.spec.secrets?.min_entropy)}
                onFilter={() => onFilter(d)}
                toggle={toggle({ id: d, what: kindInText(d) })}
              />
            );
          })
        }
      />
    </div>
  );
}
