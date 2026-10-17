import { priceListKey, type TierRow, tierErrors, tierPatch, tiersOf } from '@/features/models/tiers';
import type { DeploymentResource, EffectivePrice } from '@/lib/api/types';

/**
 * One list price in the service-tier drawer. `key` is the `pricing.models` entry, the same one the
 * Models price grid edits: `<provider_type>/<upstream_model>`, or a bare upstream id as written.
 */
export interface TierPriceRow {
  key: string;
  /** "flex=0.5, priority=2". */
  tiers: string;
  batch: string;
}

/** An upstream model the gateway serves, as a list price to set tiers on. */
export interface UpstreamChoice {
  key: string;
  upstream: string;
  deployments: string[];
}

/** A deployment that sets tiers in its own price, which win over the list price's. */
export interface OwnTiers {
  deployment: string;
  tiers: string;
  batch: string;
}

export function tierText(rows: TierRow[]): string {
  return rows.map((r) => `${r.tier}=${r.multiplier}`).join(', ');
}

/** "flex=0.5, priority=2" as rows; null when a part is not `name=value`. */
export function parseTierText(text: string): TierRow[] | null {
  const rows: TierRow[] = [];
  for (const part of text
    .split(',')
    .map((p) => p.trim())
    .filter(Boolean)) {
    const m = /^([^=\s]+)\s*=\s*(\S+)$/.exec(part);
    if (!m?.[1] || !m[2]) return null;
    rows.push({ tier: m[1], multiplier: m[2] });
  }
  return rows;
}

/** The list prices of the price settings that set tiers or a batch multiplier, and their tiers as read. */
export function tierRowsOf(models: Record<string, Record<string, unknown>> | undefined): {
  rows: TierPriceRow[];
  before: Record<string, TierRow[]>;
} {
  const rows: TierPriceRow[] = [];
  const before: Record<string, TierRow[]> = {};
  for (const [key, price] of Object.entries(models ?? {})) {
    const { tiers, batch } = tiersOf(price);
    if (tiers.length === 0 && batch === '') continue;
    rows.push({ key, tiers: tierText(tiers), batch });
    before[key] = tiers;
  }
  return { rows, before };
}

/** The upstream models the deployments serve, each with the deployments that serve it, by key. */
export function upstreamChoices(prices: EffectivePrice[]): UpstreamChoice[] {
  const byKey = new Map<string, UpstreamChoice>();
  for (const p of prices) {
    const key = priceListKey(p);
    if (!key || !p.upstream_model) continue;
    const choice = byKey.get(key) ?? { key, upstream: p.upstream_model, deployments: [] };
    choice.deployments.push(p.deployment);
    byKey.set(key, choice);
  }
  return [...byKey.values()].sort((a, b) => a.key.localeCompare(b.key));
}

export function ownTiers(deployments: DeploymentResource[]): OwnTiers[] {
  return deployments.flatMap((d) => {
    const { tiers, batch } = tiersOf(d.spec.price);
    return tiers.length > 0 || batch !== '' ? [{ deployment: d.name, tiers: tierText(tiers), batch }] : [];
  });
}

/**
 * The merge patch of `pricing.models` that makes the list prices' tiers the rows': each row's entry
 * sets its tiers (removing the ones it dropped) and batch multiplier; an entry whose row was removed
 * loses both, and keeps its token prices.
 */
export function tierSettingsPatch(
  rows: TierPriceRow[],
  before: Record<string, TierRow[]>,
): { patch: Record<string, Record<string, unknown>> } | { error: string } {
  const patch: Record<string, Record<string, unknown>> = {};
  for (const r of rows) {
    const key = r.key.trim();
    if (!key) continue;
    if (key in patch) return { error: `${key} is listed twice.` };
    const tiers = parseTierText(r.tiers);
    if (tiers === null)
      return { error: `${key}: write tiers as name=multiplier, separated by commas (flex=0.5, priority=2).` };
    const invalid = tierErrors(tiers, r.batch);
    if (invalid) return { error: `${key}: ${invalid}.` };
    if (tiers.length === 0 && !r.batch.trim() && !(key in before)) continue;
    patch[key] = tierPatch(before[key] ?? [], tiers, r.batch);
  }
  for (const key of Object.keys(before))
    if (!(key in patch)) patch[key] = { service_tiers: null, batch_multiplier: null };
  return { patch };
}
