import { z } from 'zod';

import type { ModelSpec } from '@/lib/api/types';
import { resourceName, splitList } from '@/lib/validation';

export interface AddModelValues {
  name: string;
  displayName: string;
  deploymentMode: 'new' | 'existing';
  existingDeployment: string;
  provider: string;
  upstreamModel: string;
  tier: string;
  lifecycle: 'ga' | 'preview' | 'restricted';
  labels: string;
  description: string;
  inputPrice: string;
  outputPrice: string;
  accessTeams: string;
}

export const EMPTY_ADD_MODEL: AddModelValues = {
  name: '',
  displayName: '',
  deploymentMode: 'new',
  existingDeployment: '',
  provider: '',
  upstreamModel: '',
  tier: 'balanced',
  lifecycle: 'ga',
  labels: '',
  description: '',
  inputPrice: '',
  outputPrice: '',
  accessTeams: '',
};

const label = /^[a-z0-9][a-z0-9._:-]{0,63}$/;
const optionalMoney = z
  .string()
  .trim()
  .refine((v) => v === '' || /^\d+(\.\d{1,9})?$/.test(v), 'Enter a price in US dollars, e.g. 2.50');

/** The name apps send (`gpt-4o`, `meta/llama-3.1:8b`), with errors that say what to type. */
const modelName = z
  .string()
  .trim()
  .min(1, 'Enter the name apps will send, e.g. gpt-4o-mini')
  .max(128, 'Use at most 128 characters')
  .regex(
    /^[A-Za-z0-9][A-Za-z0-9._:/-]*$/,
    'Use letters, digits and . _ : / - only, starting with a letter or digit',
  );

const labelList = z
  .string()
  .refine(
    (v) => splitList(v).every((l) => label.test(l)),
    'Use lowercase labels separated by commas, e.g. chat, vision',
  );

const teamList = z
  .string()
  .refine(
    (v) => splitList(v).every((t) => resourceName.safeParse(t).success),
    'Enter team names separated by commas, e.g. support, search',
  );

export const addModelSchema = z
  .object({
    name: modelName,
    displayName: z.string().trim().max(200, 'Use at most 200 characters'),
    deploymentMode: z.enum(['new', 'existing']),
    existingDeployment: z.string(),
    provider: z.string(),
    upstreamModel: z.string().trim(),
    tier: z.string().regex(/^[a-z][a-z0-9_-]{0,31}$/, 'Select a tier'),
    lifecycle: z.enum(['ga', 'preview', 'restricted']),
    labels: labelList,
    description: z.string().max(1024, 'Use at most 1,024 characters'),
    inputPrice: optionalMoney,
    outputPrice: optionalMoney,
    accessTeams: teamList,
  })
  .superRefine((v, ctx) => {
    if (v.deploymentMode === 'existing' && !v.existingDeployment)
      ctx.addIssue({ code: 'custom', path: ['existingDeployment'], message: 'Select a deployment' });
    if (v.deploymentMode === 'new') {
      if (!v.provider) ctx.addIssue({ code: 'custom', path: ['provider'], message: 'Select a provider' });
      if (!v.upstreamModel)
        ctx.addIssue({
          code: 'custom',
          path: ['upstreamModel'],
          message: 'Enter the model ID the provider uses, e.g. gpt-4o-mini',
        });
      if ((v.inputPrice === '') !== (v.outputPrice === ''))
        ctx.addIssue({
          code: 'custom',
          path: ['outputPrice'],
          message: 'Set both prices, or leave both blank to use list prices',
        });
    }
  });

/** A deployment name for a provider and upstream model: `openai-gpt-4o`. */
export function deploymentNameFor(provider: string, upstream: string): string {
  return `${provider}-${upstream}`
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 63);
}

export function toModelSpec(v: AddModelValues): ModelSpec {
  const labels = splitList(v.labels);
  const teams = splitList(v.accessTeams);
  return {
    ...(v.displayName.trim() ? { display_name: v.displayName.trim() } : {}),
    ...(v.description.trim() ? { description: v.description.trim() } : {}),
    tier: v.tier,
    lifecycle: { state: v.lifecycle },
    ...(labels.length > 0 ? { labels } : {}),
    ...(v.lifecycle !== 'ga' && teams.length > 0 ? { access: { teams } } : {}),
  };
}

export interface EditModelValues {
  displayName: string;
  tier: string;
  description: string;
  labels: string;
  lifecycle: 'ga' | 'preview' | 'restricted' | 'deprecated';
  eolAt: string;
  replacement: string;
  accessTeams: string;
  /** What it serves from: a tree of its own, or a shared route. */
  serves: 'own' | 'route';
  route: string;
  /** The deployment a Model leaving a shared route starts its own tree with. */
  deployment: string;
  /** Whether it had a tree of its own when the form opened. */
  hadTarget: boolean;
}

export const editModelSchema = z
  .object({
    displayName: z.string().trim().max(200, 'Use at most 200 characters'),
    tier: z.string().regex(/^[a-z][a-z0-9_-]{0,31}$/, 'Select a tier'),
    description: z.string().max(1024, 'Use at most 1,024 characters'),
    labels: labelList,
    lifecycle: z.enum(['ga', 'preview', 'restricted', 'deprecated']),
    eolAt: z.string(),
    replacement: z.string(),
    accessTeams: teamList,
    serves: z.enum(['own', 'route']),
    route: z.string(),
    deployment: z.string(),
    hadTarget: z.boolean(),
  })
  .superRefine((v, ctx) => {
    if (v.lifecycle === 'deprecated' && !v.eolAt)
      ctx.addIssue({ code: 'custom', path: ['eolAt'], message: 'Choose the date it stops working' });
    if (v.serves === 'route' && !v.route)
      ctx.addIssue({ code: 'custom', path: ['route'], message: 'Select the shared route it follows' });
    if (v.serves === 'own' && !v.hadTarget && !v.deployment)
      ctx.addIssue({ code: 'custom', path: ['deployment'], message: 'Select the deployment it sends to' });
  });

export function fromModelSpec(spec: ModelSpec): EditModelValues {
  return {
    displayName: spec.display_name ?? '',
    tier: spec.tier ?? 'balanced',
    description: spec.description ?? '',
    labels: (spec.labels ?? []).join(', '),
    lifecycle: spec.lifecycle?.state ?? 'ga',
    eolAt: spec.lifecycle?.eol_at?.slice(0, 10) ?? '',
    replacement: spec.lifecycle?.replacement ?? '',
    accessTeams: (spec.access?.teams ?? []).join(', '),
    serves: spec.route ? 'route' : 'own',
    route: spec.route ?? '',
    deployment: '',
    hadTarget: spec.target !== undefined && spec.target !== null,
  };
}

/** Applies the edit form over the Model's spec, keeping its tree, aliases and policies. */
export function applyModelEdit(spec: ModelSpec, v: EditModelValues): ModelSpec {
  const labels = splitList(v.labels);
  const teams = splitList(v.accessTeams);
  const lifecycle: NonNullable<ModelSpec['lifecycle']> = { state: v.lifecycle };
  if (v.lifecycle === 'deprecated') {
    lifecycle.eol_at = `${v.eolAt}T00:00:00Z`;
    if (v.replacement) lifecycle.replacement = v.replacement;
  }
  const {
    display_name: _d,
    description: _desc,
    labels: _l,
    access: _a,
    route: _r,
    target: _t,
    ...rest
  } = spec;
  const tree: Pick<ModelSpec, 'route' | 'target'> =
    v.serves === 'route'
      ? { route: v.route }
      : v.hadTarget
        ? { target: spec.target }
        : { target: v.deployment };
  return {
    ...rest,
    ...tree,
    tier: v.tier,
    lifecycle,
    ...(v.displayName.trim() ? { display_name: v.displayName.trim() } : {}),
    ...(v.description.trim() ? { description: v.description.trim() } : {}),
    ...(labels.length > 0 ? { labels } : {}),
    ...(teams.length > 0 || spec.access ? { access: { ...(spec.access ?? {}), teams } } : {}),
  };
}
