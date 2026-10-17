import { z } from 'zod';

import type { ProviderType } from '@/lib/api/types';

import type { ProviderSpec } from './api';

export type AuthMethod = 'api_key' | 'aws_sigv4' | 'gcp' | 'azure_ad' | 'none';

export const AUTH_LABELS: Record<AuthMethod, string> = {
  api_key: 'API key',
  aws_sigv4: 'AWS access keys',
  gcp: 'Google service account',
  azure_ad: 'Microsoft Entra ID',
  none: 'No authentication',
};

export interface ProviderValues {
  type: string;
  name: string;
  baseUrl: string;
  method: AuthMethod;
  apiKey: string;
  accessKeyId: string;
  secretAccessKey: string;
  serviceAccountJson: string;
  tenantId: string;
  clientId: string;
  clientSecret: string;
  region: string;
  gcpProject: string;
  gcpLocation: string;
}

export const EMPTY_PROVIDER: ProviderValues = {
  type: '',
  name: '',
  baseUrl: '',
  method: 'api_key',
  apiKey: '',
  accessKeyId: '',
  secretAccessKey: '',
  serviceAccountJson: '',
  tenantId: '',
  clientId: '',
  clientSecret: '',
  region: '',
  gcpProject: '',
  gcpLocation: '',
};

/** The core types first, then the rest by name. */
export function orderTypes(types: ProviderType[]): ProviderType[] {
  return [...types].sort((a, b) => Number(b.core) - Number(a.core) || a.type.localeCompare(b.type));
}

/** A free provider name for a type: `openai`, then `openai-2`, `openai-3`… */
export function suggestName(type: string, taken: string[]): string {
  const base = type.replace(/[^a-z0-9_-]+/g, '-').slice(0, 60) || 'provider';
  if (!taken.includes(base)) return base;
  for (let i = 2; i < 100; i += 1) {
    const next = `${base}-${i}`;
    if (!taken.includes(next)) return next;
  }
  return base;
}

const providerName = z
  .string()
  .trim()
  .min(1, 'Enter a name, e.g. openai')
  .max(64, 'Use at most 64 characters')
  .regex(/^[a-z0-9_-]+$/, 'Use lowercase letters, digits, - and _ only');

const url = z
  .string()
  .trim()
  .refine(
    (v) => v === '' || /^https?:\/\/[^\s/?#]+/i.test(v),
    'Enter a URL that starts with https:// or http://',
  );

/** Validation, given the selected type (whether it needs a base URL) and the names already taken. */
export function providerSchema(type: ProviderType | undefined, taken: string[]) {
  return z
    .object({
      type: z.string().min(1, 'Select a provider'),
      name: providerName.refine((v) => !taken.includes(v), 'A provider with this name already exists'),
      baseUrl: url,
      method: z.enum(['api_key', 'aws_sigv4', 'gcp', 'azure_ad', 'none']),
      apiKey: z.string(),
      accessKeyId: z.string(),
      secretAccessKey: z.string(),
      serviceAccountJson: z.string(),
      tenantId: z.string(),
      clientId: z.string(),
      clientSecret: z.string(),
      region: z.string().trim(),
      gcpProject: z.string().trim(),
      gcpLocation: z.string().trim(),
    })
    .superRefine((v, ctx) => {
      if (type?.default_base_url === null && v.baseUrl.trim() === '')
        ctx.addIssue({
          code: 'custom',
          path: ['baseUrl'],
          message: 'Enter the address of the provider’s API, e.g. https://example.com/v1',
        });
      if (v.method === 'api_key' && v.apiKey.trim() === '')
        ctx.addIssue({
          code: 'custom',
          path: ['apiKey'],
          message: 'Paste the API key the provider gave you',
        });
      if (v.method === 'aws_sigv4' && (v.accessKeyId.trim() === '') !== (v.secretAccessKey.trim() === ''))
        ctx.addIssue({
          code: 'custom',
          path: ['secretAccessKey'],
          message: 'Enter both keys, or neither to use the gateway’s own AWS credentials',
        });
      if (v.type === 'bedrock' && v.region === '')
        ctx.addIssue({ code: 'custom', path: ['region'], message: 'Enter the AWS region, e.g. us-east-1' });
      if (v.type === 'vertex' && v.gcpProject === '')
        ctx.addIssue({
          code: 'custom',
          path: ['gcpProject'],
          message: 'Enter the Google Cloud project ID',
        });
    });
}

/** The provider spec the form describes; blank optional fields are left out so defaults apply. */
export function toProviderSpec(v: ProviderValues): ProviderSpec {
  const auth: NonNullable<ProviderSpec['auth']> = { method: v.method };
  const put = (key: keyof typeof auth, value: string) => {
    if (value.trim() !== '') (auth as Record<string, string>)[key] = value.trim();
  };
  if (v.method === 'api_key') put('api_key', v.apiKey);
  if (v.method === 'aws_sigv4') {
    put('access_key_id', v.accessKeyId);
    put('secret_access_key', v.secretAccessKey);
  }
  if (v.method === 'gcp') put('service_account_json', v.serviceAccountJson);
  if (v.method === 'azure_ad') {
    put('tenant_id', v.tenantId);
    put('client_id', v.clientId);
    put('client_secret', v.clientSecret);
  }
  const spec: ProviderSpec = { type: v.type as ProviderSpec['type'], auth };
  if (v.baseUrl.trim() !== '') spec.base_url = v.baseUrl.trim();
  if (v.type === 'bedrock') spec.aws = { region: v.region.trim() };
  if (v.type === 'vertex')
    spec.gcp = {
      project: v.gcpProject.trim(),
      ...(v.gcpLocation.trim() ? { location: v.gcpLocation.trim() } : {}),
    };
  return spec;
}
