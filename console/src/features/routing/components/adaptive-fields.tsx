'use client';

import { useEffect, useState } from 'react';

import { Checkbox } from '@/components/ui/choice';
import { Field, Input, NumberInput } from '@/components/ui/form';
import { Combobox } from '@/components/ui/select';
import { api } from '@/lib/api/client';

import type { RuleDraft } from '../draft';

type Set = <K extends keyof RuleDraft>(key: K, value: RuleDraft[K]) => void;

/** The feedback metrics an adaptive split can learn from: booleans, and numbers with a range. */
export function learnable(metrics: unknown): string[] {
  if (typeof metrics !== 'object' || metrics === null) return [];
  return Object.entries(metrics as Record<string, { type?: string; min?: number; max?: number }>)
    .filter(
      ([, m]) => m.type === 'boolean' || (m.type === 'number' && m.min !== undefined && m.max !== undefined),
    )
    .map(([name]) => name)
    .sort();
}

/** The configured feedback metrics; empty when the reader can't see them, so the name is typed. */
function useFeedbackMetrics(): string[] {
  const [names, setNames] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    api.get('/settings/observability').then(
      (s) => {
        const feedback = (s.spec as { feedback?: { metrics?: unknown } }).feedback;
        if (live) setNames(learnable(feedback?.metrics));
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, []);
  return names;
}

/** An adaptive split's experiment, metric and exploration (Thompson sampling over the deployments). */
export function AdaptiveFields({
  draft,
  set,
  errors,
}: {
  draft: RuleDraft;
  set: Set;
  errors: Record<string, string>;
}) {
  const metrics = useFeedbackMetrics();
  const a = draft.adaptive;
  const patch = (p: Partial<RuleDraft['adaptive']>) => set('adaptive', { ...a, ...p });
  return (
    <fieldset className="flex flex-col gap-4">
      <legend className="mb-2 text-ui font-medium text-fg">Learning</legend>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field
          label="Experiment"
          error={errors['adaptive.experiment']}
          hint="Its name in the request log and in feedback."
        >
          <Input mono value={a.experiment} onChange={(e) => patch({ experiment: e.target.value })} />
        </Field>
        <Field
          label="Learns from"
          error={errors['adaptive.metric']}
          hint="A feedback metric: thumbs up or down, or a score with a range."
        >
          {metrics.length > 0 ? (
            <Combobox
              aria-label="Feedback metric"
              value={a.metric}
              placeholder="Select a metric"
              onChange={(v) => patch({ metric: v })}
              options={metrics.map((m) => ({ value: m, label: m }))}
            />
          ) : (
            <Input mono value={a.metric} onChange={(e) => patch({ metric: e.target.value })} />
          )}
        </Field>
        <Field
          label="Explore first"
          error={errors['adaptive.minSamples']}
          hint="Feedback each deployment gets in turn before the split learns."
        >
          <NumberInput
            min={1}
            max={100000}
            className="w-28"
            value={a.minSamples}
            onChange={(v) => patch({ minSamples: v })}
          />
        </Field>
      </div>
      <Checkbox
        label="Stop learning once one deployment clearly wins"
        description="Every later request then goes to it."
        checked={a.stop}
        onChange={(v) => patch({ stop: v })}
      />
      {a.stop ? (
        <Field
          label="Chance it is not the best, at most"
          error={errors['adaptive.delta']}
          hint="Percent; 5 means the winner is the best with 95% probability."
        >
          <NumberInput
            min={0}
            max={50}
            step={0.5}
            className="w-28"
            value={a.delta}
            onChange={(v) => patch({ delta: v })}
          />
        </Field>
      ) : null}
    </fieldset>
  );
}
