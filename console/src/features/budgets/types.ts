import type { BudgetCounter } from '@/lib/api/types';

export type BudgetState = 'blocked' | 'over' | 'at_risk' | 'on_pace';
export type BudgetLevel = 'organization' | 'team' | 'project' | 'key';

/**
 * What a row stands for: a budget; a partitioned budget (one counter per user, key or other value,
 * shown as a group with its partitions inside); one partition; or the "Unassigned" group of an
 * organization's per-user budget (people whose team has no budget of its own).
 */
export type RowKind = 'budget' | 'partitioned' | 'partition' | 'unassigned';

export interface BudgetRow {
  id: string;
  counter: BudgetCounter;
  kind: RowKind;
  level: string;
  owner: string;
  name: string;
  /** A partition row's attribute and value (`user`, `ada`); the empty value is the shared counter. */
  partition: { attr: string; value: string } | null;
  /** How many partitions a group holds. */
  partitions: number;
  spent: number;
  amount: number;
  /** The amount admission refuses at: the amount plus running grants. */
  effective: number;
  reserved: number;
  projected: number;
  /** Fraction of the period elapsed, for the pacing mark. */
  elapsed: number;
  state: BudgetState;
  /** A rolling window (`algorithm: sliding_window`): no period end, so no forecast. */
  rolling: boolean;
  /** The person the declaration names as its owner. */
  budgetOwner: string | null;
  /** The forecast's spend per day (the trailing 7 days of the period), when the gateway reports it. */
  ratePerDay: number | null;
  runsOutAt: string | null;
  /** The id of the row this one sits in, if any. */
  parentId: string | null;
  children: BudgetRow[];
}

/** What the tree needs to know about declarations and people, beyond the counters. */
export interface TreeContext {
  parentOf: (level: string, owner: string) => { level: string; owner: string } | null;
  /** The declaration of a budget, as its owner states it. */
  declaration?: (level: string, owner: string, name: string) => Record<string, unknown> | undefined;
  /** A user's primary team, for placing per-user seats of an organization budget. */
  teamOfUser?: (user: string) => string | undefined;
}

export function budgetId(c: Pick<BudgetCounter, 'level' | 'owner' | 'name' | 'partition'>): string {
  return `${c.level}/${c.owner}/${c.name}${c.partition !== null ? `/${c.partition}` : ''}`;
}

/** `user=ada` → `{attr: 'user', value: 'ada'}`; several attributes are joined with ", ". */
export function parsePartition(text: string | null | undefined): { attr: string; value: string } | null {
  if (text === null || text === undefined) return null;
  const parts = text
    .split(',')
    .filter(Boolean)
    .map((p) => {
      const i = p.indexOf('=');
      return i < 0 ? { attr: p, value: '' } : { attr: p.slice(0, i), value: p.slice(i + 1) };
    });
  if (parts.length === 0) return null;
  return { attr: parts.map((p) => p.attr).join(', '), value: parts.map((p) => p.value).join(', ') };
}

export const PARTITION_NOUN: Record<string, [string, string]> = {
  user: ['person', 'people'],
  key: ['key', 'keys'],
  end_user: ['end user', 'end users'],
  project: ['project', 'projects'],
  team: ['team', 'teams'],
  tag: ['tag', 'tags'],
};

export function partitionNoun(attr: string, count: number): string {
  const [one, many] = PARTITION_NOUN[attr] ?? ['value', 'values'];
  return count === 1 ? one : many;
}

/** A partition's label: its value, or what the shared counter counts when the value is empty. */
export function partitionLabel(p: { attr: string; value: string }): string {
  if (p.value !== '') return p.value;
  return `Without a ${partitionNoun(p.attr, 1)}`;
}

/**
 * The period's linear projection: spend so far scaled to the whole period. A period without an end
 * (lifetime) projects to what is spent.
 */
export function project(counter: BudgetCounter, now: number): { projected: number; elapsed: number } {
  const spent = Number(counter.spent);
  const start = new Date(counter.period_start).getTime();
  const end = counter.resets_at ? new Date(counter.resets_at).getTime() : NaN;
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start)
    return { projected: spent, elapsed: 1 };
  const elapsed = Math.min(1, Math.max(0.01, (now - start) / (end - start)));
  const reported = counter.forecast ? Number(counter.forecast.projected_spend) : NaN;
  return { projected: Number.isFinite(reported) ? reported : spent / elapsed, elapsed };
}

/** The gateway's state when it reports one, else one derived from spend and the projection. */
export function stateOf(counter: BudgetCounter, projected: number): BudgetState {
  if (counter.state) return counter.state;
  const spent = Number(counter.spent);
  const amount = Number(counter.amount);
  if (amount > 0 && spent >= amount)
    return counter.mode === 'hard' || counter.mode === 'approval' ? 'blocked' : 'over';
  if (amount > 0 && projected > amount) return 'at_risk';
  return 'on_pace';
}

export const STATE_LABEL: Record<BudgetState, string> = {
  blocked: 'Blocked',
  over: 'Over budget',
  at_risk: 'At risk',
  on_pace: 'On track',
};

export const LEVEL_LABEL: Record<string, string> = {
  gateway: 'Gateway',
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
  key: 'Key',
  model: 'Model',
  route: 'Route',
  node: 'Node',
};

export const EVENT_LABEL: Record<string, string> = {
  threshold_crossed: 'Reached an alert threshold',
  exhausted: 'Reached its limit',
  forecast_exceeded: 'Projected to go over',
  overage_granted: 'Allowed to go over',
  overage_requested: 'Asked to go over',
  reset: 'Period reset',
  adjusted: 'Amount spent adjusted',
  config_changed: 'Budget changed',
};

/** What happens when the budget is reached, in a few words. */
export function enforcementLabel(counter: BudgetCounter): string {
  if (counter.downgrade) {
    const to =
      counter.downgrade.to_model ??
      (counter.downgrade.to_tier ? `the ${counter.downgrade.to_tier} tier` : '');
    return `Switches to ${to} at ${Math.round(counter.downgrade.at * 100)}%`;
  }
  if (counter.mode === 'hard') return 'Blocks at the limit';
  if (counter.mode === 'approval') return 'Blocks, then asks to approve';
  return 'Alerts only';
}

/**
 * When the budget runs out, in words: already, when it is spent to its limit (approved overage
 * included); the forecast's date; or not this period. A rolling budget has no forecast.
 */
export function runsOutWords(
  row: Pick<BudgetRow, 'spent' | 'effective' | 'state' | 'rolling' | 'runsOutAt'>,
  date: (iso: string) => string,
): string {
  if (row.state === 'blocked' || (row.effective > 0 && row.spent >= row.effective)) return 'Already reached';
  if (row.runsOutAt) return date(row.runsOutAt);
  return row.rolling ? '—' : 'Not this period';
}

export function stateTone(state: BudgetState): 'error' | 'warn' | 'success' {
  return state === 'blocked' || state === 'over' ? 'error' : state === 'at_risk' ? 'warn' : 'success';
}

const ORDER: Record<BudgetState, number> = { blocked: 0, over: 1, at_risk: 2, on_pace: 3 };

function worst(rows: BudgetRow[]): BudgetState {
  return rows.reduce<BudgetState>((w, r) => (ORDER[r.state] < ORDER[w] ? r.state : w), 'on_pace');
}

function rowOf(c: BudgetCounter, now: number, ctx: TreeContext | undefined): BudgetRow {
  const { projected, elapsed } = project(c, now);
  const decl = ctx?.declaration?.(c.level, c.owner, c.name);
  // A gateway reports no forecast only for a rolling budget, so the counter says so even when the
  // declaration can't be read.
  const rolling =
    decl?.algorithm === 'sliding_window' || (!c.forecast && !c.resets_at && c.state !== undefined);
  const rate = c.forecast ? Number(c.forecast.rate_per_day) : NaN;
  const partition = parsePartition(c.partition);
  return {
    id: budgetId(c),
    counter: c,
    kind: partition ? 'partition' : 'budget',
    level: c.level,
    owner: c.owner,
    name: c.name,
    partition,
    partitions: 0,
    spent: Number(c.spent),
    amount: Number(c.amount),
    effective: Number(c.effective_amount ?? c.amount),
    reserved: Number(c.reserved),
    projected: rolling ? Number(c.spent) : projected,
    elapsed: rolling ? 1 : elapsed,
    state: stateOf(c, projected),
    rolling,
    budgetOwner: c.budget_owner ?? null,
    ratePerDay: Number.isFinite(rate) ? rate : null,
    runsOutAt: rolling ? null : (c.forecast?.runs_out_at ?? null),
    parentId: null,
    children: [],
  };
}

/** A group row over rows: sums of the money, the worst state. */
function groupOf(
  kind: 'partitioned' | 'unassigned',
  id: string,
  members: BudgetRow[],
  first: BudgetRow,
): BudgetRow {
  const sum = (f: (r: BudgetRow) => number) => members.reduce((a, r) => a + f(r), 0);
  return {
    ...first,
    id,
    kind,
    counter: { ...first.counter, partition: null },
    partition: null,
    partitions: members.length,
    spent: sum((r) => r.spent),
    amount: sum((r) => r.amount),
    effective: sum((r) => r.effective),
    reserved: sum((r) => r.reserved),
    projected: sum((r) => r.projected),
    state: worst(members),
    ratePerDay: null,
    runsOutAt: null,
    children: members,
  };
}

/**
 * Builds the organization → team → project → key tree from flat counters, using the hierarchy to
 * place each owner under its parent's budget. Budgets whose parent has none sit at the top. A
 * partitioned budget is one row with its partitions inside; the per-user partitions of an
 * organization budget sit under the person's team budget, or under "Unassigned" when the team has
 * none.
 */
export function buildTree(
  counters: BudgetCounter[],
  parentOrContext: TreeContext['parentOf'] | TreeContext,
  now: number,
): BudgetRow[] {
  const ctx: TreeContext =
    typeof parentOrContext === 'function' ? { parentOf: parentOrContext } : parentOrContext;
  const groups = new Map<string, BudgetRow[]>();
  const rows: BudgetRow[] = [];
  for (const c of counters) {
    const row = rowOf(c, now, ctx);
    if (row.kind === 'partition') {
      const key = `${c.level}/${c.owner}/${c.name}`;
      const list = groups.get(key);
      if (list) list.push(row);
      else {
        groups.set(key, [row]);
        rows.push(row);
      }
    } else rows.push(row);
  }
  const nodes = rows.map((r) => {
    if (r.kind !== 'partition') return r;
    const key = `${r.level}/${r.owner}/${r.name}`;
    const members = groups.get(key) ?? [r];
    const group = groupOf('partitioned', key, members, r);
    for (const m of members) m.parentId = group.id;
    return group;
  });
  const byOwner = new Map<string, BudgetRow>();
  // An owner's first budget hosts the budgets inside it; a budget of all values is preferred.
  for (const r of nodes)
    if (r.kind === 'budget' && !byOwner.has(`${r.level}/${r.owner}`)) byOwner.set(`${r.level}/${r.owner}`, r);
  for (const r of nodes) if (!byOwner.has(`${r.level}/${r.owner}`)) byOwner.set(`${r.level}/${r.owner}`, r);
  const roots: BudgetRow[] = [];
  for (const r of nodes) {
    let parent = ctx.parentOf(r.level, r.owner);
    let host: BudgetRow | undefined;
    while (parent && !host) {
      host = byOwner.get(`${parent.level}/${parent.owner}`);
      if (!host) parent = ctx.parentOf(parent.level, parent.owner);
    }
    if (host && host !== r) {
      host.children.push(r);
      r.parentId = host.id;
    } else roots.push(r);
  }
  // Seats of an organization budget: under the person's team budget, else "Unassigned".
  for (const g of nodes) {
    if (g.kind !== 'partitioned' || g.level !== 'organization' || g.children[0]?.partition?.attr !== 'user')
      continue;
    const kept: BudgetRow[] = [];
    const unassigned: BudgetRow[] = [];
    for (const p of g.children) {
      const team = p.partition?.value ? ctx.teamOfUser?.(p.partition.value) : undefined;
      const host = team ? byOwner.get(`team/${team}`) : undefined;
      if (host) {
        host.children.push(p);
        p.parentId = host.id;
      } else if (p.partition?.value) unassigned.push(p);
      else kept.push(p);
    }
    const first = unassigned[0];
    if (first && ctx.teamOfUser) {
      const u = groupOf('unassigned', `${g.id}/unassigned`, unassigned, first);
      u.parentId = g.id;
      for (const p of unassigned) p.parentId = u.id;
      g.children = [...kept, u];
    } else g.children = [...kept, ...unassigned];
    for (const p of g.children) if (p.kind === 'partition') p.parentId = g.id;
  }
  const sort = (list: BudgetRow[]) => {
    list.sort(
      (a, b) =>
        Number(a.kind === 'partition') - Number(b.kind === 'partition') ||
        ORDER[a.state] - ORDER[b.state] ||
        b.projected - a.projected,
    );
    for (const r of list) sort(r.children);
  };
  sort(roots);
  return roots;
}

export function flatten(rows: BudgetRow[]): BudgetRow[] {
  return rows.flatMap((r) => [r, ...flatten(r.children)]);
}

/** Rows that are budgets of their own (a budget or one partition), not groups. */
export function isCounted(row: BudgetRow): boolean {
  return row.kind === 'budget' || row.kind === 'partition';
}

/** Every row by id, for finding a row's parent. */
export function indexRows(rows: BudgetRow[]): Map<string, BudgetRow> {
  return new Map(flatten(rows).map((r) => [r.id, r]));
}

/** The budget most in need of attention: blocked, then over, then at risk by projected overage. */
export function topIssue(rows: BudgetRow[]): BudgetRow | null {
  const all = flatten(rows).filter((r) => isCounted(r) && r.state !== 'on_pace');
  all.sort((a, b) => ORDER[a.state] - ORDER[b.state] || b.projected - b.amount - (a.projected - a.amount));
  return all[0] ?? null;
}
