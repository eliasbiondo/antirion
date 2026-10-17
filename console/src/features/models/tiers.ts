/**
 * Request-dependent prices: multipliers of a price's token rates by
 * the service tier the provider served, and for Batches API usage.
 */

/**
 * The list-price entry a deployment's tiers are set on, `<provider_type>/<upstream_model>`: the key
 * of `pricing.models`. Never the public Model name, which may differ from the upstream id.
 */
export function priceListKey(
  price: { provider_type?: string | null; upstream_model?: string | null } | null | undefined,
): string | null {
  return price?.provider_type && price.upstream_model
    ? `${price.provider_type}/${price.upstream_model}`
    : null;
}

export interface TierRow {
  tier: string;
  multiplier: string;
}

const TIER = /^[a-z0-9_-]{1,64}$/;
const DECIMAL = /^\d+(\.\d+)?$/;

/** `service_tiers` and `batch_multiplier` of a price, as read from the API (strings, or absent). */
export function tiersOf(price: Record<string, unknown> | null | undefined): {
  tiers: TierRow[];
  batch: string;
} {
  const raw = price?.service_tiers;
  const tiers =
    raw && typeof raw === 'object' && !Array.isArray(raw)
      ? Object.entries(raw as Record<string, unknown>)
          .filter(([, v]) => typeof v === 'string' || typeof v === 'number')
          .map(([tier, v]) => ({ tier, multiplier: String(v) }))
      : [];
  const batch = price?.batch_multiplier;
  return { tiers, batch: typeof batch === 'string' || typeof batch === 'number' ? String(batch) : '' };
}

/** "flex ×0.5 · priority ×2". */
export function tiersText(rows: TierRow[]): string {
  return rows.map((r) => `${r.tier} ×${r.multiplier}`).join(' · ');
}

export function tierErrors(rows: TierRow[], batch: string): string | null {
  const seen = new Set<string>();
  for (const r of rows) {
    const t = r.tier.trim();
    if (!TIER.test(t)) return 'Name each tier as the provider does, e.g. flex or priority';
    if (seen.has(t)) return `Tier ${t} is listed twice`;
    seen.add(t);
    if (!DECIMAL.test(r.multiplier.trim())) return `Tier ${t}: a multiplier such as 0.5 or 2`;
  }
  if (batch.trim() && !DECIMAL.test(batch.trim())) return 'The batch multiplier is a number such as 0.5';
  return null;
}

/**
 * The merge patch of one list price's tiers: the tiers set, removed tiers as null (a merge patch
 * keeps what it doesn't name), and the batch multiplier or null.
 */
export function tierPatch(before: TierRow[], rows: TierRow[], batch: string): Record<string, unknown> {
  const next = Object.fromEntries(rows.map((r) => [r.tier.trim(), r.multiplier.trim()]));
  const removed = before.filter((b) => !(b.tier in next)).map((b) => [b.tier, null] as const);
  return {
    service_tiers: rows.length === 0 ? null : { ...Object.fromEntries(removed), ...next },
    batch_multiplier: batch.trim() || null,
  };
}
