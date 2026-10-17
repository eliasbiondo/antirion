import type { Me, Session } from '@/lib/api/types';

/**
 * Whether the principal is a person with an account (profile, preferences, second factors,
 * personal tokens, notifications). Admin tokens and the bootstrap credential have none of these:
 * the gateway answers 403 to every `/auth/me/*` call they make, so the console does not ask.
 */
export function hasAccount(me: Me | null): boolean {
  return me?.principal.type === 'user';
}

/** How the principal signed in, in words: "Bootstrap admin", "Admin token", or the person's name. */
export function principalName(me: Me | null): string {
  if (!me) return '';
  const user = me.user && typeof me.user === 'object' ? me.user : null;
  if (user) return user.display_name || user.name;
  if (me.principal.type === 'bootstrap') return 'Bootstrap admin';
  return me.principal.id;
}

/** `gateway_admin` → "Gateway admin". */
export function roleLabel(role: string | null): string | null {
  if (!role) return null;
  const words = role.replace(/[_-]+/g, ' ').trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const METHOD: Record<string, string> = {
  token: 'Admin token',
  local: 'Password',
  oidc: 'Single sign-on',
  saml: 'Single sign-on',
  oauth: 'GitHub',
};

export function methodLabel(method: Session['method']): string {
  return METHOD[method] ?? method;
}

const BROWSERS: [RegExp, string][] = [
  [/Edg(e|A|iOS)?\//, 'Edge'],
  [/OPR\/|Opera/, 'Opera'],
  [/Firefox\/|FxiOS\//, 'Firefox'],
  [/Chrome\/|CriOS\//, 'Chrome'],
  [/Version\/[\d.]+.*Safari\//, 'Safari'],
  [/^curl\//, 'curl'],
  [/^antirion\//i, 'Antirion CLI'],
];

const SYSTEMS: [RegExp, string][] = [
  [/iPhone|iPad|iPod/, 'iOS'],
  [/Android/, 'Android'],
  [/CrOS/, 'ChromeOS'],
  [/Mac OS X|Macintosh/, 'macOS'],
  [/Windows/, 'Windows'],
  [/Linux/, 'Linux'],
];

/** "Chrome on macOS" from a user agent; anything unrecognised is shown as it came. */
export function describeAgent(agent: string | null | undefined): string {
  if (!agent) return 'Unknown device';
  const browser = BROWSERS.find(([re]) => re.test(agent))?.[1];
  const system = SYSTEMS.find(([re]) => re.test(agent))?.[1];
  if (browser && system) return `${browser} on ${system}`;
  if (browser) return browser;
  return agent.length > 48 ? `${agent.slice(0, 47)}…` : agent;
}

/** "São Paulo, Brazil" from a session's GeoIP location; null when there is none. */
export function describeLocation(
  location: { country: string | null; region: string | null; city: string | null } | null | undefined,
): string | null {
  if (!location) return null;
  let country = location.country;
  if (country) {
    try {
      country = new Intl.DisplayNames(['en'], { type: 'region' }).of(country) ?? country;
    } catch {
      // An unknown code is shown as it came.
    }
  }
  const parts = [location.city, country ?? location.region].filter((p): p is string => Boolean(p));
  return parts.length > 0 ? parts.join(', ') : null;
}
