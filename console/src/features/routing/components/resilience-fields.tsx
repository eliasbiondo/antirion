'use client';

import { Button, IconButton } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/choice';
import { Field, Input } from '@/components/ui/form';
import { Select } from '@/components/ui/select';

import {
  blankPredicate,
  type HedgingDraft,
  PREDICATE_KINDS,
  type PredicateDraft,
  type PredicateKind,
} from '../resilience';

function Legend({ children }: { children: string }) {
  return <legend className="mb-2 text-ui font-medium text-fg">{children}</legend>;
}

/** Hedged requests: a second attempt on another deployment when the first is slow. */
export function HedgingFields({
  value,
  onChange,
  errors,
  prefix = '',
  disabled,
}: {
  value: HedgingDraft;
  onChange: (v: HedgingDraft) => void;
  errors: Record<string, string>;
  prefix?: string;
  disabled?: boolean;
}) {
  return (
    <fieldset className="flex flex-col gap-3">
      <Legend>Hedged requests</Legend>
      <Checkbox
        label="Also ask another deployment when the answer is slow"
        description="The first answer wins and the others are cancelled. Every attempt is charged, so this trades cost for tail latency. Never for requests with side effects."
        checked={value.enabled}
        disabled={disabled}
        onChange={(enabled) => onChange({ ...value, enabled })}
      />
      {value.enabled ? (
        <div className="grid gap-4 sm:grid-cols-3">
          <Field
            label="After"
            error={errors[`${prefix}hedging.delay`]}
            hint="Without an answer for this long; at least 10ms."
          >
            <Input
              mono
              aria-label="Hedge after"
              disabled={disabled}
              value={value.delay}
              placeholder="2s"
              onChange={(e) => onChange({ ...value, delay: e.target.value })}
            />
          </Field>
          <Field label="Extra requests at most" hint="Each counts toward the attempts.">
            <Select
              aria-label="Extra requests at most"
              disabled={disabled}
              value={value.maxExtra}
              onChange={(v) => onChange({ ...value, maxExtra: v === '2' ? '2' : '1' })}
              options={[
                { value: '1', label: '1' },
                { value: '2', label: '2' },
              ]}
            />
          </Field>
        </div>
      ) : null}
    </fieldset>
  );
}

/** Answers that count as failures: a header value, a finish reason, an empty answer. */
export function ResponseChecks({
  value,
  onChange,
  errors,
  prefix = '',
  disabled,
}: {
  value: PredicateDraft[];
  onChange: (v: PredicateDraft[]) => void;
  errors: Record<string, string>;
  prefix?: string;
  disabled?: boolean;
}) {
  const update = (i: number, patch: Partial<PredicateDraft>) =>
    onChange(value.map((p, j) => (j === i ? { ...p, ...patch } : p)));
  return (
    <fieldset className="flex flex-col gap-2">
      <Legend>Answers that count as failures</Legend>
      <p className="-mt-1 text-caption text-muted">
        A successful answer that matches one of these is treated as a server error: the next deployment is
        tried, and the deployment’s health is not affected. Finish reasons and empty answers are checked on
        answers that are not streamed.
      </p>
      {value.map((p, i) => (
        <div key={i} className="flex flex-col gap-1">
          <div className="grid grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)_minmax(0,1fr)_auto] items-start gap-2 max-sm:grid-cols-2">
            <Select
              aria-label={`Check ${i + 1}`}
              disabled={disabled}
              value={p.kind}
              onChange={(v) => update(i, { kind: v as PredicateKind })}
              options={PREDICATE_KINDS}
            />
            {p.kind === 'header' ? (
              <>
                <Input
                  mono
                  aria-label={`Header of check ${i + 1}`}
                  placeholder="x-fail"
                  disabled={disabled}
                  value={p.header}
                  onChange={(e) => update(i, { header: e.target.value })}
                />
                <Input
                  mono
                  aria-label={`Value of check ${i + 1}`}
                  placeholder="1"
                  disabled={disabled}
                  value={p.equals}
                  onChange={(e) => update(i, { equals: e.target.value })}
                />
              </>
            ) : p.kind === 'finish_reason' ? (
              <Input
                mono
                className="col-span-2 max-sm:col-span-1"
                aria-label={`Finish reasons of check ${i + 1}`}
                placeholder="length, content_filter"
                disabled={disabled}
                value={p.reasons}
                onChange={(e) => update(i, { reasons: e.target.value })}
              />
            ) : (
              <span className="col-span-2 self-center text-caption text-muted max-sm:col-span-1">
                No text and no tool calls.
              </span>
            )}
            <IconButton
              icon="x"
              label={`Remove check ${i + 1}`}
              disabled={disabled}
              onClick={() => onChange(value.filter((_, j) => j !== i))}
            />
          </div>
          {errors[`${prefix}predicates.${i}`] ? (
            <p className="text-caption text-error">{errors[`${prefix}predicates.${i}`]}</p>
          ) : null}
        </div>
      ))}
      <Button
        size="sm"
        icon="plus"
        className="self-start"
        disabled={disabled}
        onClick={() => onChange([...value, blankPredicate()])}
      >
        Add check
      </Button>
    </fieldset>
  );
}
