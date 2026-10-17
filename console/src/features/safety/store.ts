import { create } from 'zustand';

import type {
  GuardrailResource,
  ModelResource,
  SafetyEvent,
  TeamResource,
  UsageBucket,
} from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  type GuardrailRuntime,
  type Interval,
  type PoliciesSettings,
  type ReviewState,
  safetyApi,
  type SummaryRow,
  type TopicShare,
} from './api';
import {
  type Check,
  configChecks,
  type Doc,
  generationsByDay,
  mfaCheck,
  rotationCheck,
  score,
} from './posture';
import type { GuardrailSpec } from './rules';

/** What the screen may read, so it never asks for what the principal can't see. */
export interface SafetyReads {
  policies: boolean;
  summary: boolean;
  usage: boolean;
  config: boolean;
  users: boolean;
  keys: boolean;
  teams: boolean;
  models: boolean;
}

export interface Posture {
  checks: Check[];
  score: number | null;
  /** One score per day over the last 30 days, oldest first; null where no configuration was known. */
  series: (number | null)[];
}

interface SafetyState {
  events: Slot<SafetyEvent[]>;
  guardrails: Slot<GuardrailResource[]>;
  runtime: Slot<GuardrailRuntime[]>;
  /** Topic-restriction findings of the last 7 days by topic, whatever the range shown. */
  topics: Slot<TopicShare[]>;
  /** The gateway's policies: where guardrails run at the gateway level. */
  policies: Slot<PoliciesSettings>;
  byCategory: Slot<SummaryRow[]>;
  byEntity: Slot<Record<string, number>>;
  lastHour: Slot<UsageBucket[]>;
  teams: Slot<TeamResource[]>;
  models: Slot<ModelResource[]>;
  posture: Slot<Posture>;
  load: (
    from: string,
    to: string,
    options?: { quiet?: boolean; interval?: Interval; reads?: Partial<SafetyReads> },
  ) => Promise<void>;
  loadPosture: (reads: Partial<SafetyReads>) => Promise<void>;
  /** Creates or replaces a guardrail, then sets where it runs at the gateway level. */
  saveRule: (input: {
    name: string;
    spec: GuardrailSpec;
    original: GuardrailResource | null;
    attach: { input: boolean; output: boolean } | null;
  }) => Promise<void>;
  deleteRule: (name: string) => Promise<void>;
  /** Replaces a guardrail's settings (a preset), with the ETag it was read with. */
  updateRule: (rule: GuardrailResource, spec: GuardrailSpec) => Promise<void>;
  review: (id: string, state: ReviewState) => Promise<void>;
  setEnabled: (name: string, enabled: boolean) => Promise<void>;
}

/** Safety events in the range, the guardrails and their 24-hour figures. */
export const useSafety = create<SafetyState>()((set, get) => ({
  events: emptySlot(),
  guardrails: emptySlot(),
  runtime: emptySlot(),
  topics: emptySlot(),
  policies: emptySlot(),
  byCategory: emptySlot(),
  byEntity: emptySlot(),
  lastHour: emptySlot(),
  teams: emptySlot(),
  models: emptySlot(),
  posture: emptySlot(),
  async load(from, to, options) {
    const reads = options?.reads ?? {};
    const interval = options?.interval ?? '1h';
    await Promise.all([
      loadSlot(set, get, 'topics', () => safetyApi.topics(), options),
      loadSlot(set, get, 'events', () => safetyApi.events(from, to), options),
      loadSlot(set, get, 'guardrails', safetyApi.guardrails, options),
      loadSlot(set, get, 'runtime', safetyApi.runtime, options),
      reads.policies ? loadSlot(set, get, 'policies', safetyApi.policies, options) : null,
      reads.summary
        ? loadSlot(set, get, 'byCategory', () => safetyApi.byCategory(from, to, interval), options)
        : null,
      reads.summary ? loadSlot(set, get, 'byEntity', () => safetyApi.byEntity(), options) : null,
      reads.usage ? loadSlot(set, get, 'lastHour', () => safetyApi.lastHour(), options) : null,
      reads.teams && !get().teams.data ? loadSlot(set, get, 'teams', safetyApi.teams) : null,
      reads.models && !get().models.data ? loadSlot(set, get, 'models', safetyApi.models) : null,
    ]);
  },
  async loadPosture(reads) {
    await loadSlot(set, get, 'posture', async () => {
      const [doc, users, keys, generations] = await Promise.all([
        safetyApi.config(),
        reads.users ? safetyApi.users().catch(() => null) : Promise.resolve(null),
        reads.keys ? safetyApi.keys().catch(() => null) : Promise.resolve(null),
        safetyApi.generations().catch(() => []),
      ]);
      const people = mfaCheck(
        users?.map((u) => ({
          kind: u.spec.kind,
          enabled: u.spec.enabled,
          lastLogin: u.metadata.last_login_at ?? null,
          method: u.metadata.last_login_method ?? null,
          mfa: u.metadata.mfa_methods ?? [],
        })) ?? null,
      );
      const rotation = rotationCheck(
        keys ? keys.filter((k) => k.metadata.rotation_due).length : null,
        keys?.length ?? 0,
      );
      const checks = [...configChecks(doc), people, rotation];
      // Past days are read from the configuration of each day; people and keys as they are today.
      const byDay = generationsByDay(generations, Date.now());
      const docs = new Map<number, Doc | null>();
      for (const n of [...new Set(byDay.filter((g): g is number => g !== null))].slice(-30))
        docs.set(n, await safetyApi.generation(n).catch(() => null));
      const series = byDay.map((g, i) => {
        if (i === byDay.length - 1) return score(checks);
        const past = g === null ? null : (docs.get(g) ?? null);
        return past ? score([...configChecks(past), people, rotation]) : null;
      });
      return { checks, score: score(checks), series };
    });
  },
  async saveRule({ name, spec, original, attach }) {
    if (original) await safetyApi.replace(name, spec, original.metadata.etag);
    else await safetyApi.create(name, spec);
    if (attach) {
      const policies = get().policies.data ?? (await safetyApi.policies());
      const lists = policies.spec.guardrails ?? {};
      // A rule already listed keeps its place: the order is the order guardrails run in.
      const update = (list: string[] | undefined, on: boolean) => {
        const current = list ?? [];
        if (on) return current.includes(name) ? current : [...current, name];
        return current.filter((n) => n !== name);
      };
      const input = update(lists.input, attach.input);
      const output = update(lists.output, attach.output);
      const same = (a: string[] | undefined, b: string[]) => JSON.stringify(a ?? []) === JSON.stringify(b);
      if (!same(lists.input, input) || !same(lists.output, output))
        await safetyApi.attach({ input, output }, policies.metadata.etag);
      await loadSlot(set, get, 'policies', safetyApi.policies, { quiet: true });
    }
    await loadSlot(set, get, 'guardrails', safetyApi.guardrails, { quiet: true });
  },
  async updateRule(rule, spec) {
    await safetyApi.replace(rule.name, spec, rule.metadata.etag);
    await loadSlot(set, get, 'guardrails', safetyApi.guardrails, { quiet: true });
  },
  async deleteRule(name) {
    const policies = get().policies.data;
    const lists = policies?.spec.guardrails;
    if (policies && lists && (lists.input?.includes(name) || lists.output?.includes(name))) {
      await safetyApi.attach(
        {
          input: (lists.input ?? []).filter((n) => n !== name),
          output: (lists.output ?? []).filter((n) => n !== name),
        },
        policies.metadata.etag,
      );
      await loadSlot(set, get, 'policies', safetyApi.policies, { quiet: true });
    }
    await safetyApi.remove(name);
    await loadSlot(set, get, 'guardrails', safetyApi.guardrails, { quiet: true });
  },
  async review(id, state) {
    const updated = await safetyApi.review(id, state);
    set((s) => ({
      events: { ...s.events, data: (s.events.data ?? []).map((e) => (e.id === id ? updated : e)) },
    }));
  },
  async setEnabled(name, enabled) {
    const g = get().guardrails.data?.find((x) => x.name === name);
    await safetyApi.setEnabled(name, enabled, g?.metadata.etag);
    await loadSlot(set, get, 'guardrails', safetyApi.guardrails, { quiet: true });
  },
}));

export const CATEGORY_LABEL: Record<string, string> = {
  pii: 'PII',
  prompt_injection: 'Injection & jailbreak',
  secret: 'Secrets',
  toxic: 'Toxic content',
  custom: 'Custom',
  routing_block: 'Blocked by routing',
  off_topic: 'Off topic',
  deny_pattern: 'Pattern',
  allow_pattern: 'Outside allow-list',
  keyword: 'Keyword',
  hate: 'Hate',
  harassment: 'Harassment',
  violence: 'Violence',
  sexual: 'Sexual',
  self_harm: 'Self-harm',
  illicit: 'Illicit',
  invalid_json: 'Invalid JSON',
  schema_violation: 'Schema',
  tool_not_allowed: 'Tool not allowed',
  length: 'Length',
  uninspectable: 'Uninspectable',
};

export function severityTone(severity: string): 'error' | 'warn' | 'info' | 'neutral' {
  return severity === 'critical'
    ? 'error'
    : severity === 'high'
      ? 'warn'
      : severity === 'medium'
        ? 'info'
        : 'neutral';
}
