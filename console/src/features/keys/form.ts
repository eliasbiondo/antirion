import { z } from 'zod';

import type { KeySpec } from '@/lib/api/types';
import { cidr, modelName, resourceName, splitList } from '@/lib/validation';

import { ENDPOINT_FAMILIES, KEY_PERMISSIONS } from './types';

export type ExpiryChoice = 'never' | '7d' | '30d' | '90d' | '365d' | 'custom';

/** How a key is bound to a client certificate: not at all, by the certificate's SHA-256, or by a name in it. */
export type CertBinding = 'none' | 'fingerprint' | 'san';

export interface KeyFormValues {
  name: string;
  description: string;
  project: string;
  owner: string;
  expiry: ExpiryChoice;
  expiresOn: string;
  endpoints: string[];
  permissions: string[];
  models: string;
  allowedIps: string;
  /** Named IP sets of the gateway's settings, written as `set:<name>` entries of `allowed_ips`. */
  ipSets: string[];
  regions: string[];
  denyRegions: string[];
  /** The class a limit's priority shares give this key's requests. */
  priorityClass: string;
  /** The client certificate a request with this key must present over mutual TLS. */
  certBinding: CertBinding;
  /** The fingerprint or the subject alternative name the binding matches. */
  certValue: string;
  rpm: string;
  tpm: string;
}

export const EMPTY_KEY_FORM: KeyFormValues = {
  name: '',
  description: '',
  project: 'default',
  owner: '',
  expiry: '90d',
  expiresOn: '',
  endpoints: [],
  permissions: [],
  models: '',
  allowedIps: '',
  ipSets: [],
  regions: [],
  denyRegions: [],
  priorityClass: '',
  certBinding: 'none',
  certValue: '',
  rpm: '',
  tpm: '',
};

const FINGERPRINT = /^([0-9A-Fa-f]{64}|([0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2})$/;

const optionalCount = z
  .string()
  .trim()
  .refine((v) => v === '' || /^\d+$/.test(v), 'Enter a whole number, or leave it blank for no limit');

export const keyFormSchema = z
  .object({
    name: resourceName,
    description: z.string().max(500, 'At most 500 characters'),
    project: resourceName,
    owner: z.string(),
    expiry: z.enum(['never', '7d', '30d', '90d', '365d', 'custom']),
    expiresOn: z.string(),
    endpoints: z.array(z.enum(ENDPOINT_FAMILIES)),
    permissions: z.array(z.enum(KEY_PERMISSIONS.map((p) => p.id) as [string, ...string[]])),
    models: z
      .string()
      .refine(
        (v) => splitList(v).every((m) => m === '*' || modelName.safeParse(m.replace(/\*$/, 'x')).success),
        'Enter model names separated by commas, e.g. gpt-4o, claude-*',
      ),
    allowedIps: z
      .string()
      .refine(
        (v) => splitList(v).every((ip) => cidr.safeParse(ip).success),
        'Enter one IP address or range per line, e.g. 10.0.0.0/8',
      ),
    ipSets: z.array(z.string()),
    regions: z.array(z.string()),
    denyRegions: z.array(z.string()),
    priorityClass: z
      .string()
      .trim()
      .refine((v) => v === '' || /^[a-z0-9_]{1,64}$/.test(v), 'Lowercase letters, digits and _, up to 64'),
    certBinding: z.enum(['none', 'fingerprint', 'san']),
    certValue: z.string(),
    rpm: optionalCount,
    tpm: optionalCount,
  })
  .refine((v) => v.certBinding !== 'fingerprint' || FINGERPRINT.test(v.certValue.trim()), {
    message: 'Enter the SHA-256 fingerprint: 64 hexadecimal digits, with or without colons',
    path: ['certValue'],
  })
  .refine(
    (v) => v.certBinding !== 'san' || (v.certValue.trim() !== '' && v.certValue.trim().length <= 1024),
    {
      message: 'Enter the name the certificate carries, e.g. spiffe://acme.test/billing',
      path: ['certValue'],
    },
  )
  .refine(
    (v) => v.expiry !== 'custom' || (v.expiresOn !== '' && new Date(v.expiresOn).getTime() > Date.now()),
    {
      message: 'Choose a date in the future',
      path: ['expiresOn'],
    },
  );

const DAYS: Record<Exclude<ExpiryChoice, 'never' | 'custom'>, number> = {
  '7d': 7,
  '30d': 30,
  '90d': 90,
  '365d': 365,
};

function expiresAt(values: KeyFormValues, now: number): string | undefined {
  if (values.expiry === 'never') return undefined;
  if (values.expiry === 'custom')
    return new Date(`${values.expiresOn}T23:59:59Z`).toISOString().replace(/\.\d{3}Z$/, 'Z');
  return new Date(now + DAYS[values.expiry] * 86_400_000).toISOString().replace(/\.\d{3}Z$/, 'Z');
}

/**
 * The key spec the form describes, written over `base` so fields the form does not show (tags,
 * metadata, regions, other limits) survive an edit unchanged.
 */
export function toKeySpec(
  values: KeyFormValues,
  base: KeySpec = {},
  now = Date.now(),
  keepExpiry = false,
): KeySpec {
  const limits: Record<string, unknown> = { ...(base.policies?.limits ?? {}) };
  if (values.rpm.trim()) limits.rpm = { unit: 'requests', limit: Number(values.rpm), per: '1m' };
  else limits.rpm = undefined;
  if (values.tpm.trim()) limits.tpm = { unit: 'tokens', limit: Number(values.tpm), per: '1m' };
  else limits.tpm = undefined;
  const policies: Record<string, unknown> = { ...(base.policies ?? {}), limits: compact(limits) };
  const spec: Record<string, unknown> = {
    ...base,
    project: values.project,
    description: values.description.trim() || undefined,
    owner: values.owner || undefined,
    expires_at: keepExpiry ? base.expires_at : expiresAt(values, now),
    endpoints: values.endpoints,
    permissions: values.permissions,
    models: splitList(values.models),
    allowed_ips: [...splitList(values.allowedIps), ...values.ipSets.map((n) => `set:${n}`)],
    regions: values.regions,
    deny_regions: values.denyRegions,
    priority_class: values.priorityClass.trim() || undefined,
    require_client_cert: clientCert(values),
    policies: compact(policies),
  };
  return compact(spec) as KeySpec;
}

function clientCert(values: KeyFormValues): KeySpec['require_client_cert'] {
  const value = values.certValue.trim();
  if (values.certBinding === 'fingerprint') return { sha256_fingerprint: value };
  if (values.certBinding === 'san') return { san: value };
  return undefined;
}

/** Drops undefined values, empty arrays and empty objects, so the spec states only what is set. */
function compact<T extends Record<string, unknown>>(record: T): T | undefined {
  const entries = Object.entries(record).filter(([, v]) => {
    if (v === undefined) return false;
    if (Array.isArray(v)) return v.length > 0;
    if (v !== null && typeof v === 'object') return Object.keys(v).length > 0;
    return true;
  });
  return entries.length > 0 ? (Object.fromEntries(entries) as T) : undefined;
}

/** The form values for an existing key. */
export function fromKeySpec(name: string, spec: KeySpec): KeyFormValues {
  const limits = (spec.policies?.limits ?? {}) as Record<string, { limit?: number } | undefined>;
  return {
    name,
    description: spec.description ?? '',
    project: spec.project ?? 'default',
    owner: spec.owner ?? '',
    expiry: spec.expires_at ? 'custom' : 'never',
    expiresOn: spec.expires_at ? spec.expires_at.slice(0, 10) : '',
    endpoints: (spec.endpoints ?? []).filter((e) => e !== '*'),
    permissions: spec.permissions ?? [],
    models: (spec.models ?? []).join(', '),
    allowedIps: (spec.allowed_ips ?? []).filter((ip) => !ip.startsWith('set:')).join('\n'),
    ipSets: (spec.allowed_ips ?? []).filter((ip) => ip.startsWith('set:')).map((ip) => ip.slice(4)),
    regions: spec.regions ?? [],
    denyRegions: spec.deny_regions ?? [],
    priorityClass: spec.priority_class ?? '',
    certBinding: spec.require_client_cert?.sha256_fingerprint
      ? 'fingerprint'
      : spec.require_client_cert?.san
        ? 'san'
        : 'none',
    certValue: spec.require_client_cert?.sha256_fingerprint ?? spec.require_client_cert?.san ?? '',
    rpm: limits.rpm?.limit !== undefined ? String(limits.rpm.limit) : '',
    tpm: limits.tpm?.limit !== undefined ? String(limits.tpm.limit) : '',
  };
}
