import { create } from 'zustand';

import type { components } from '@/lib/api/schema';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { type GovernanceSettings, type GuardrailResource, settingsApi } from './api';

type S = components['schemas'];
export type PolicyObservability = NonNullable<S['PoliciesSettings']['spec']['observability']>;
export type PolicyGuardrails = NonNullable<S['PoliciesSettings']['spec']['guardrails']>;

/** The policy level Data & privacy edits: the gateway's own, or the scope's organization's. */
export interface PolicyLevel {
  organization: string | null;
  observability: PolicyObservability;
  guardrails: PolicyGuardrails;
  etag: string | undefined;
}

/** A level below the edited one that keeps no content (zero data retention). */
export interface ZdrLevel {
  kind: 'organization' | 'team' | 'project' | 'key';
  name: string;
}

interface PrivacyState {
  policy: Slot<PolicyLevel>;
  governance: Slot<GovernanceSettings>;
  guardrails: Slot<GuardrailResource[]>;
  zdr: Slot<ZdrLevel[]>;
  load: (organization: string | null) => Promise<void>;
  loadPolicy: (organization: string | null) => Promise<void>;
  /** Merges `patch` into the level's `policies` (observability, guardrails). */
  savePolicy: (patch: Record<string, unknown>) => Promise<void>;
  loadGuardrails: () => Promise<void>;
}

async function readPolicy(organization: string | null): Promise<PolicyLevel> {
  if (organization) {
    const org = await settingsApi.organization(organization);
    return {
      organization,
      observability: org.spec.policies?.observability ?? {},
      guardrails: org.spec.policies?.guardrails ?? {},
      etag: org.metadata.etag,
    };
  }
  const policies = await settingsApi.policies();
  return {
    organization: null,
    observability: policies.spec.observability ?? {},
    guardrails: policies.spec.guardrails ?? {},
    etag: policies.metadata.etag,
  };
}

type WithPolicies = { name: string; spec: { policies?: { observability?: PolicyObservability } | null } };

function zeroRetention(kind: ZdrLevel['kind'], items: WithPolicies[]): ZdrLevel[] {
  return items
    .filter((i) => i.spec.policies?.observability?.zero_data_retention === true)
    .map((i) => ({ kind, name: i.name }));
}

/** Where zero data retention is on below the gateway: organizations, teams, projects and keys. */
async function readZdr(organization: string | null): Promise<ZdrLevel[]> {
  const [orgs, teams, projects, keys] = await Promise.all([
    organization ? Promise.resolve([]) : settingsApi.organizations().catch(() => []),
    settingsApi.teams().catch(() => []),
    settingsApi.projects().catch(() => []),
    settingsApi.keys().catch(() => []),
  ]);
  const inScope = (team: string | undefined) =>
    organization === null || teams.find((t) => t.name === team)?.spec.organization === organization;
  const projectTeam = new Map(projects.map((p) => [p.name, p.spec.team]));
  return [
    ...zeroRetention('organization', orgs as WithPolicies[]),
    ...zeroRetention(
      'team',
      (teams as WithPolicies[]).filter((t) => inScope(t.name)),
    ),
    ...zeroRetention(
      'project',
      projects.filter((p) => inScope(p.spec.team)),
    ),
    ...zeroRetention(
      'key',
      keys.filter((k) => inScope(projectTeam.get(k.spec.project ?? ''))),
    ),
  ];
}

/** Data & privacy: the level's content and redaction policy, protected paths, custom entities. */
export const usePrivacy = create<PrivacyState>()((set, get) => ({
  policy: emptySlot(),
  governance: emptySlot(),
  guardrails: emptySlot(),
  zdr: emptySlot(),

  async load(organization) {
    await Promise.all([
      get().loadPolicy(organization),
      loadSlot(set, get, 'governance', settingsApi.governance),
      get().loadGuardrails(),
      loadSlot(set, get, 'zdr', () => readZdr(organization)),
    ]);
  },

  loadPolicy: (organization) => loadSlot(set, get, 'policy', () => readPolicy(organization)),

  loadGuardrails: () => loadSlot(set, get, 'guardrails', settingsApi.guardrails),

  async savePolicy(patch) {
    const level = get().policy.data;
    if (!level) return;
    if (level.organization) {
      await settingsApi.patchOrganization(level.organization, { policies: patch }, level.etag);
    } else {
      await settingsApi.patchPolicies(patch, level.etag);
    }
    await loadSlot(set, get, 'policy', () => readPolicy(level.organization), { quiet: true });
  },
}));
