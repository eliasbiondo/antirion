'use client';

import { Checkbox } from '@/components/ui/choice';
import { Field, Input, NumberInput, Segmented } from '@/components/ui/form';
import { ModelMark } from '@/components/ui/marks';
import { Combobox, Select } from '@/components/ui/select';
import type { DeploymentResource } from '@/lib/api/types';

import type { RuleDraft } from '../draft';
import { isRecord, type Json } from '../types';

type Set = <K extends keyof RuleDraft>(key: K, value: RuleDraft[K]) => void;
type Errors = Record<string, string>;

function Legend({ children }: { children: string }) {
  return <legend className="mb-2 text-ui font-medium text-fg">{children}</legend>;
}

/** Who picks a pool's replica (the gateway, or an external endpoint picker) and LoRA adapter affinity. */
export function PoolFields({ draft, set, errors }: { draft: RuleDraft; set: Set; errors: Errors }) {
  const p = draft.pool;
  const patch = (x: Partial<RuleDraft['pool']>) => set('pool', { ...p, ...x });
  return (
    <fieldset className="flex flex-col gap-4">
      <Legend>Picking a replica</Legend>
      <Field label="Who picks">
        <Segmented<'builtin' | 'external'>
          label="Who picks"
          value={p.picker}
          onChange={(picker) => patch({ picker })}
          options={[
            { value: 'builtin', label: 'The gateway' },
            { value: 'external', label: 'An endpoint picker' },
          ]}
        />
      </Field>
      {p.picker === 'builtin' ? (
        <p className="-mt-2 text-caption text-muted">
          The gateway reads each replica’s queue and cache from its metrics and prefers the one that has seen
          the prompt’s beginning.
        </p>
      ) : (
        <>
          <p className="-mt-2 text-caption text-muted">
            An inference scheduler that speaks the endpoint-picker protocol (gRPC external processing). It is
            asked before each request and answers with the replica’s address; the gateway tells it which
            replica served.
          </p>
          <div className="grid gap-4 sm:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
            <Field label="Picker address" error={errors['pool.url']} required>
              <Input
                mono
                value={p.url}
                placeholder="http://picker.internal:9002"
                onChange={(e) => patch({ url: e.target.value })}
              />
            </Field>
            <Field label="Answer within" error={errors['pool.timeout']} hint="10ms to 30s; default 300ms.">
              <Input
                mono
                value={p.timeout}
                placeholder="300ms"
                onChange={(e) => patch({ timeout: e.target.value })}
              />
            </Field>
          </div>
          <Field label="When the picker fails" hint="Unreachable, too slow, or an answer it can’t read.">
            <Select
              value={p.onError}
              onChange={(v) => patch({ onError: v === 'builtin' ? 'builtin' : 'reject' })}
              options={[
                { value: 'reject', label: 'Refuse the request (503)' },
                { value: 'builtin', label: 'Let the gateway pick, and mark the answer degraded' },
              ]}
            />
          </Field>
        </>
      )}
      <Checkbox
        label="Keep requests for an adapter on the replicas that run it"
        description="For LoRA adapters: replicas already serving the adapter first, then those with a free adapter slot, then any."
        checked={p.lora}
        onChange={(lora) => patch({ lora })}
      />
    </fieldset>
  );
}

function scorerKind(original: Json): string | null {
  return isRecord(original) && isRecord(original.scorer) && typeof original.scorer.kind === 'string'
    ? original.scorer.kind
    : null;
}

const SCORER_LABEL: Record<string, string> = {
  random: 'Random',
  external: 'A scoring webhook',
  classifier: 'A keyword classifier',
  llm: 'A judge model',
};

/** The strong and weak models of a cost-quality rule and its learned (matrix-factorisation) scorer. */
export function CostQualityFields({
  draft,
  set,
  errors,
  deployments,
}: {
  draft: RuleDraft;
  set: Set;
  errors: Errors;
  deployments: DeploymentResource[];
}) {
  const c = draft.costQuality;
  const patch = (x: Partial<RuleDraft['costQuality']>) => set('costQuality', { ...c, ...x });
  const options = deployments.map((d) => ({
    value: d.name,
    label: `${d.name} · ${d.spec.provider}/${d.spec.model}`,
    icon: <ModelMark model={null} upstream={[d.spec.base_model, d.spec.model]} size="md" />,
  }));
  const other = scorerKind(draft.original);
  const keptKind = other && other !== 'matrix_factorization' ? other : null;
  return (
    <div className="flex flex-col gap-5">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Strong model" error={errors['costQuality.strong']} hint="Better answers, higher price.">
          <Combobox
            aria-label="Strong deployment"
            value={c.strong}
            placeholder="Select a deployment"
            onChange={(strong) => patch({ strong })}
            options={options}
          />
        </Field>
        <Field label="Weak model" error={errors['costQuality.weak']} hint="Cheaper; takes the rest.">
          <Combobox
            aria-label="Weak deployment"
            value={c.weak}
            placeholder="Select a deployment"
            onChange={(weak) => patch({ weak })}
            options={options}
          />
        </Field>
      </div>
      <div className="grid gap-4 sm:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)]">
        <Field label="Send to the strong model">
          <Segmented<'threshold' | 'fraction'>
            label="Send to the strong model"
            value={c.by}
            onChange={(by) => patch({ by })}
            options={[
              { value: 'threshold', label: 'Prompts scored above' },
              { value: 'fraction', label: 'A share of prompts' },
            ]}
          />
        </Field>
        <Field
          label={c.by === 'threshold' ? 'Score, in percent' : 'Share, in percent'}
          error={errors['costQuality.percent']}
          hint={
            c.by === 'threshold'
              ? 'How likely the strong model answers better.'
              : 'The highest-rated prompts, measured on live traffic.'
          }
        >
          <NumberInput
            min={0}
            max={100}
            className="w-28"
            value={c.percent}
            onChange={(percent) => patch({ percent })}
          />
        </Field>
      </div>
      <fieldset className="flex flex-col gap-3">
        <Legend>Scorer</Legend>
        {keptKind ? (
          <Field label="Scored by">
            <Select
              value={c.scorer}
              onChange={(v) => patch({ scorer: v === 'kept' ? 'kept' : 'matrix_factorization' })}
              options={[
                { value: 'kept', label: `${SCORER_LABEL[keptKind] ?? keptKind}, as configured` },
                { value: 'matrix_factorization', label: 'A learned scorer (matrix factorisation)' },
              ]}
            />
          </Field>
        ) : null}
        {c.scorer === 'matrix_factorization' ? (
          <>
            <p className="text-caption text-muted">
              A pre-trained model file rates each prompt from its embedding, with no training per request. The
              gateway reads the file when the configuration is applied.
            </p>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Model file" error={errors['costQuality.modelFile']} required>
                <Input
                  mono
                  value={c.modelFile}
                  placeholder="/etc/antirion/mf-router.json"
                  onChange={(e) => patch({ modelFile: e.target.value })}
                />
              </Field>
              <Field
                label="Embeddings model"
                error={errors['costQuality.embeddingModel']}
                required
                hint="A Model of this gateway that embeds the prompt."
              >
                <Input
                  mono
                  value={c.embeddingModel}
                  placeholder="text-embedding-3-small"
                  onChange={(e) => patch({ embeddingModel: e.target.value })}
                />
              </Field>
              <Field
                label="Strong model’s name in the file"
                error={errors['costQuality.strongName']}
                required
              >
                <Input mono value={c.strongName} onChange={(e) => patch({ strongName: e.target.value })} />
              </Field>
              <Field label="Weak model’s name in the file" error={errors['costQuality.weakName']} required>
                <Input mono value={c.weakName} onChange={(e) => patch({ weakName: e.target.value })} />
              </Field>
            </div>
          </>
        ) : null}
        <Field label="When scoring fails" hint="The embedding call fails or times out.">
          <Select
            value={c.onError}
            onChange={(v) => patch({ onError: v as RuleDraft['costQuality']['onError'] })}
            options={[
              { value: 'strong', label: 'Send to the strong model' },
              { value: 'weak', label: 'Send to the weak model' },
              { value: 'reject', label: 'Refuse the request (503)' },
            ]}
          />
        </Field>
      </fieldset>
    </div>
  );
}
