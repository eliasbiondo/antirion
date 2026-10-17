import type { UserResource } from '@/lib/api/types';

import type { Spend } from './api';

/** A person's month in figures, with what marks them out. */
export interface Person {
  name: string;
  displayName: string;
  title: string | null;
  team: string | null;
  service: boolean;
  lastActive: string | null;
  requests: number;
  cost: number;
  /** Spend per request, null without requests. */
  perRequest: number | null;
  /** Spending more than twice the mean of the people of the same team who spent. */
  outlier: boolean;
}

/**
 * People with their requests, spend and cost per request this month. An outlier spends more than
 * twice the mean spend of the people of their primary team who spent anything (at least two of
 * them); people without a team are compared with each other.
 */
export function peopleFacts(users: UserResource[], usage: Map<string, Spend>): Person[] {
  const rows = users.map<Person>((u) => {
    const s = usage.get(u.name) ?? { requests: 0, cost: 0 };
    return {
      name: u.name,
      displayName: u.spec.display_name ?? u.name,
      title: u.spec.title ?? null,
      team: u.spec.primary_team ?? null,
      service: u.spec.kind === 'service',
      lastActive: u.metadata.last_active_at ?? null,
      requests: s.requests,
      cost: s.cost,
      perRequest: s.requests > 0 ? s.cost / s.requests : null,
      outlier: false,
    };
  });
  const byTeam = new Map<string, Person[]>();
  for (const p of rows) {
    if (p.cost <= 0) continue;
    const key = p.team ?? '';
    byTeam.set(key, [...(byTeam.get(key) ?? []), p]);
  }
  for (const group of byTeam.values()) {
    if (group.length < 2) continue;
    const mean = group.reduce((a, p) => a + p.cost, 0) / group.length;
    for (const p of group) p.outlier = p.cost > 2 * mean;
  }
  return rows.sort((a, b) => b.cost - a.cost || a.name.localeCompare(b.name));
}

/**
 * Where the People tab's figures come from: only requests made with keys a person owns count
 * toward them, so traffic on service or unowned keys shows here as nobody's.
 */
export function peopleNote(people: Person[]): string {
  const base = 'Requests and spend count the keys each person owns.';
  return people.length > 0 && people.every((p) => p.requests === 0)
    ? `${base} None of them made a request this month: so far the traffic came from keys no person owns, such as service keys.`
    : `${base} Traffic on keys no person owns, such as service keys, is not counted here.`;
}
