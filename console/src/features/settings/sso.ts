import type { components } from '@/lib/api/schema';
import type { FieldErrors } from '@/lib/validation';

import type { UserResource } from './api';
import { NAME_RE, URL_RE } from './form-errors';

type AuthSpec = components['schemas']['AuthSettings']['spec'];
export type Sso = NonNullable<NonNullable<AuthSpec['admin']>['sso']>;
export type OidcSpec = NonNullable<Sso['oidc']>[string];
export type SamlSpec = NonNullable<Sso['saml']>[string];
export type OauthSpec = NonNullable<Sso['oauth']>[string];

/** The configuration's key of each sign-in protocol: OIDC, SAML and GitHub (an OAuth provider). */
export type Protocol = 'oidc' | 'saml' | 'oauth';

export const PROTOCOL_LABEL: Record<Protocol, string> = { oidc: 'OIDC', saml: 'SAML', oauth: 'GitHub' };

/** The sign-in method a user's last sign-in names for each protocol. */
const METHOD: Record<Protocol, string> = { oidc: 'oidc', saml: 'saml', oauth: 'oauth' };

export interface ProviderRow {
  protocol: Protocol;
  name: string;
  displayName: string;
  spec: OidcSpec | SamlSpec | OauthSpec;
}

/** Every configured single sign-on provider, by protocol then name. */
export function providerRows(sso: Sso | undefined): ProviderRow[] {
  const rows: ProviderRow[] = [];
  for (const protocol of ['oidc', 'saml', 'oauth'] as const) {
    for (const [name, spec] of Object.entries(
      (sso?.[protocol] ?? {}) as Record<string, ProviderRow['spec']>,
    )) {
      rows.push({ protocol, name, displayName: spec.display_name ?? name, spec });
    }
  }
  return rows;
}

const DAY_MS = 86_400_000;

/**
 * People whose last sign-in in the last 30 days went through a provider. A sign-in recorded before
 * the gateway kept the provider's name counts for its protocol's provider when that is the only one.
 */
export function signInsByProvider(
  users: UserResource[],
  rows: ProviderRow[],
  now: number,
): Map<string, number> {
  const out = new Map<string, number>();
  const since = now - 30 * DAY_MS;
  for (const u of users) {
    const at = u.metadata.last_login_at ? Date.parse(u.metadata.last_login_at) : NaN;
    if (!(at >= since)) continue;
    let row = rows.find(
      (r) => r.name === u.metadata.last_login_provider && METHOD[r.protocol] === u.metadata.last_login_method,
    );
    if (!row && !u.metadata.last_login_provider) {
      const same = rows.filter((r) => METHOD[r.protocol] === u.metadata.last_login_method);
      if (same.length === 1) row = same[0];
    }
    if (row) out.set(`${row.protocol}/${row.name}`, (out.get(`${row.protocol}/${row.name}`) ?? 0) + 1);
  }
  return out;
}

export interface Mapping {
  /** The claim (OIDC), attribute (SAML) or GitHub organization. */
  match: string;
  /** The claim or attribute value; for GitHub, the team (blank for the whole organization). */
  value: string;
  role: string;
  scope: string;
}

export interface Binding {
  role: string;
  scope: string;
}

export interface ProviderForm {
  protocol: Protocol;
  name: string;
  displayName: string;
  jit: boolean;
  domains: string[];
  mappings: Mapping[];
  defaults: Binding[];
  issuer: string;
  clientId: string;
  /** Blank on an existing provider keeps the stored secret. */
  clientSecret: string;
  scopes: string[];
  apiBase: string;
  orgs: string[];
  idpSource: 'metadata_url' | 'metadata' | 'fields';
  metadataUrl: string;
  metadata: string;
  entityId: string;
  ssoUrl: string;
  certificates: string;
  refresh: string;
  spEntityId: string;
  nameId: 'email' | 'persistent' | 'transient' | 'unspecified';
  emailAttribute: string;
  idpInitiated: boolean;
}

export const DEFAULT_SCOPES = ['openid', 'email', 'profile', 'groups'];

/** The form for a provider, or a blank one for `protocol`. */
export function providerForm(
  protocol: Protocol,
  name: string | null,
  spec: ProviderRow['spec'] | undefined,
): ProviderForm {
  const oidc = protocol === 'oidc' ? (spec as OidcSpec | undefined) : undefined;
  const saml = protocol === 'saml' ? (spec as SamlSpec | undefined) : undefined;
  const oauth = protocol === 'oauth' ? (spec as OauthSpec | undefined) : undefined;
  const idp = saml?.idp;
  const mappings: Mapping[] = oidc
    ? (oidc.role_mappings ?? []).map((m) => ({
        match: m.claim,
        value: m.value,
        role: m.role,
        scope: m.scope,
      }))
    : saml
      ? (saml.role_mappings ?? []).map((m) => ({
          match: m.attribute,
          value: m.value,
          role: m.role,
          scope: m.scope,
        }))
      : (oauth?.role_mappings ?? []).map((m) => ({
          match: m.organization,
          value: m.team ?? '',
          role: m.role,
          scope: m.scope,
        }));
  return {
    protocol,
    name: name ?? '',
    displayName: spec?.display_name ?? '',
    jit: spec?.jit_provisioning ?? true,
    domains: oidc?.allowed_email_domains ?? saml?.allowed_email_domains ?? [],
    mappings,
    defaults: (oidc?.default_role_bindings ?? saml?.default_role_bindings ?? []).map((b) => ({ ...b })),
    issuer: oidc?.issuer ?? '',
    clientId: oidc?.client_id ?? oauth?.client_id ?? '',
    clientSecret: '',
    scopes: oidc?.scopes ?? DEFAULT_SCOPES,
    apiBase: oauth?.api_base_url ?? '',
    orgs: oauth?.allowed_organizations ?? [],
    idpSource: idp?.metadata_url
      ? 'metadata_url'
      : idp?.metadata
        ? 'metadata'
        : idp?.entity_id
          ? 'fields'
          : 'metadata_url',
    metadataUrl: idp?.metadata_url ?? '',
    metadata: idp?.metadata ?? '',
    entityId: idp?.entity_id ?? '',
    ssoUrl: idp?.sso_url ?? '',
    certificates: (idp?.certificates ?? []).join('\n\n'),
    refresh: idp?.metadata_refresh ?? '24h',
    spEntityId: saml?.sp_entity_id ?? '',
    nameId: saml?.name_id_format ?? 'email',
    emailAttribute: saml?.email_attribute ?? '',
    idpInitiated: saml?.allow_idp_initiated ?? false,
  };
}

/** A PEM bundle (or bare base64 blocks separated by blank lines) as a list of certificates. */
export function splitCertificates(text: string): string[] {
  const pem = text.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g);
  if (pem) return pem.map((c) => c.trim());
  return text
    .split(/\n\s*\n/)
    .map((c) => c.trim())
    .filter(Boolean);
}

/** The provider's settings as the configuration takes them; the secret only when one was typed. */
export function providerSpec(f: ProviderForm): Record<string, unknown> {
  const common: Record<string, unknown> = {
    ...(f.displayName.trim() ? { display_name: f.displayName.trim() } : {}),
    jit_provisioning: f.jit,
  };
  const bindings = f.defaults.filter((b) => b.role && b.scope);
  if (f.protocol === 'oidc') {
    return {
      ...common,
      issuer: f.issuer.trim(),
      client_id: f.clientId.trim(),
      ...(f.clientSecret ? { client_secret: f.clientSecret } : {}),
      scopes: f.scopes,
      ...(f.domains.length ? { allowed_email_domains: f.domains } : {}),
      role_mappings: f.mappings.map((m) => ({
        claim: m.match.trim(),
        value: m.value,
        role: m.role,
        scope: m.scope,
      })),
      default_role_bindings: bindings,
    };
  }
  if (f.protocol === 'oauth') {
    return {
      ...common,
      type: 'github',
      client_id: f.clientId.trim(),
      ...(f.clientSecret ? { client_secret: f.clientSecret } : {}),
      ...(f.apiBase.trim() ? { api_base_url: f.apiBase.trim() } : {}),
      allowed_organizations: f.orgs,
      role_mappings: f.mappings.map((m) => ({
        organization: m.match.trim(),
        ...(m.value.trim() ? { team: m.value.trim() } : {}),
        role: m.role,
        scope: m.scope,
      })),
    };
  }
  const idp: Record<string, unknown> =
    f.idpSource === 'metadata_url'
      ? { metadata_url: f.metadataUrl.trim(), metadata_refresh: f.refresh.trim() || '24h' }
      : f.idpSource === 'metadata'
        ? { metadata: f.metadata.trim() }
        : {
            entity_id: f.entityId.trim(),
            sso_url: f.ssoUrl.trim(),
            certificates: splitCertificates(f.certificates),
          };
  return {
    ...common,
    idp,
    ...(f.spEntityId.trim() ? { sp_entity_id: f.spEntityId.trim() } : {}),
    name_id_format: f.nameId,
    ...(f.emailAttribute.trim() ? { email_attribute: f.emailAttribute.trim() } : {}),
    allow_idp_initiated: f.idpInitiated,
    ...(f.domains.length ? { allowed_email_domains: f.domains } : {}),
    role_mappings: f.mappings.map((m) => ({
      attribute: m.match.trim(),
      value: m.value,
      role: m.role,
      scope: m.scope,
    })),
    default_role_bindings: bindings,
  };
}

/** What the form lacks before it can be saved, by field ("mappings.0.role" for a row's role). */
export function providerErrors(f: ProviderForm, isNew: boolean, taken: string[]): FieldErrors {
  const e: FieldErrors = {};
  if (isNew) {
    if (!f.name.trim()) e.name = 'Enter a name';
    else if (!NAME_RE.test(f.name)) e.name = 'Use letters, digits, . _ and -; start with a letter or digit';
    else if (taken.includes(f.name)) e.name = 'A provider already has this name';
  }
  if (f.displayName.length > 64) e.displayName = 'Use at most 64 characters';
  if (f.protocol === 'oidc') {
    if (!URL_RE.test(f.issuer.trim()))
      e.issuer = 'Enter the issuer address, such as https://login.example.com';
    if (!f.clientId.trim()) e.clientId = 'Enter the client ID';
  }
  if (f.protocol === 'oauth') {
    if (!f.clientId.trim()) e.clientId = 'Enter the client ID';
    if (isNew && !f.clientSecret) e.clientSecret = 'Enter the client secret';
    if (f.apiBase.trim() && !URL_RE.test(f.apiBase.trim()))
      e.apiBase = 'Enter an address such as https://api.github.com';
  }
  if (f.protocol === 'saml') {
    if (f.idpSource === 'metadata_url' && !URL_RE.test(f.metadataUrl.trim()))
      e.metadataUrl = 'Enter the metadata address your identity provider gives';
    if (f.idpSource === 'metadata' && !f.metadata.trim().startsWith('<'))
      e.metadata = "Paste the identity provider's metadata XML";
    if (f.idpSource === 'fields') {
      if (!f.entityId.trim()) e.entityId = "Enter the identity provider's entity ID";
      if (!URL_RE.test(f.ssoUrl.trim())) e.ssoUrl = 'Enter the single sign-on address';
      if (splitCertificates(f.certificates).length === 0)
        e.certificates = 'Paste at least one signing certificate';
    }
  }
  f.mappings.forEach((m, i) => {
    if (!m.match.trim())
      e[`mappings.${i}.match`] =
        f.protocol === 'oauth'
          ? 'Enter an organization'
          : f.protocol === 'saml'
            ? 'Enter an attribute'
            : 'Enter a claim';
    if (!m.role) e[`mappings.${i}.role`] = 'Choose a role';
    if (!m.scope) e[`mappings.${i}.scope`] = 'Choose where it applies';
  });
  f.defaults.forEach((b, i) => {
    if (!b.role) e[`defaults.${i}.role`] = 'Choose a role';
    if (!b.scope) e[`defaults.${i}.scope`] = 'Choose where it applies';
  });
  return e;
}

/** A role binding scope in words: "Whole gateway", "Organization acme", "Team platform". */
export function scopeWords(scope: string): string {
  if (scope === 'gateway') return 'Whole gateway';
  const [kind, name] = scope.split('/');
  const label = { organization: 'Organization', team: 'Team', project: 'Project' }[kind ?? ''] ?? kind;
  return `${label} ${name ?? ''}`.trim();
}
