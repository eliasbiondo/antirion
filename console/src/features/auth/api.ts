import { api, API_BASE, ApiError } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type { LoginOptions, Me } from '@/lib/api/types';

export type MfaVerify = components['schemas']['MfaVerify'];

/** An emailed link's state (null when the gateway could not say) and, while valid, what it is for. */
export interface LinkCheck {
  state: 'valid' | 'expired' | 'used' | 'unknown' | null;
  email: string | null;
  organization: string | null;
  expiresAt: string | null;
}

/** What a gateway without `GET /auth/login_options` offers: the admin-token form only. */
export const TOKEN_ONLY: LoginOptions = {
  methods: [{ type: 'token', id: 'token' }],
  password_reset: false,
  organization: null,
  version: null,
  ticker: null,
  trusted_device: { enabled: false, max_age_days: null },
  password: null,
  setup_required: false,
  device_authorization: false,
};

export const authApi = {
  async loginOptions(organization: string | undefined): Promise<LoginOptions> {
    try {
      return await api.get('/auth/login_options', { query: organization ? { organization } : {} });
    } catch {
      return TOKEN_ONLY;
    }
  },
  localLogin: (body: { email: string; password: string; organization?: string }) =>
    api.post('/auth/local/login', { body }),
  localMfa: (body: MfaVerify): Promise<Me> => api.post('/auth/local/mfa', { body }),
  enrollTotp: (challenge: string) => api.post('/auth/me/mfa/totp', { undeclared: { challenge } }),
  enrollWebauthn: (challenge: string) => api.post('/auth/me/mfa/webauthn', { undeclared: { challenge } }),
  /** Confirming the first factor with the sign-in challenge starts the session and returns the recovery codes. */
  confirmTotp: (body: { challenge: string; factor_id: string; totp: string }) =>
    api.post('/auth/me/mfa/totp:confirm', { body }),
  confirmWebauthn: (body: { challenge: string; factor_id: string; webauthn: Record<string, unknown> }) =>
    api.post('/auth/me/mfa/webauthn:confirm', { body }),
  me: (): Promise<Me> => api.get('/auth/me'),
  /** Follows the link mailed to a new email: the change it confirms takes effect. */
  confirmEmail: (token: string): Promise<Me> => api.patch('/auth/me', { body: { email_token: token } }),
  requestReset: (email: string) => api.post('/auth/local/password_reset', { body: { email } }),
  acceptInvitation: (token: string, password: string) =>
    api.post('/auth/invitations:accept', { body: { token, password } }),
  confirmReset: (token: string, newPassword: string) =>
    api.post('/auth/local/password_reset:confirm', { body: { token, new_password: newPassword } }),
  /**
   * What an emailed link is, read without using it. Anything but an answer (throttling, a network
   * error, a gateway without the check) leaves the state open, and the form is shown.
   */
  async checkLink(purpose: 'reset' | 'invitation', token: string): Promise<LinkCheck> {
    try {
      if (purpose === 'invitation') {
        const r = await api.post('/auth/invitations:check', { body: { token } });
        return {
          state: r.state,
          email: r.email ?? null,
          organization: r.organization?.display_name || r.organization?.name || null,
          expiresAt: r.expires_at ?? null,
        };
      }
      const r = await api.post('/auth/local/password_reset:check', { body: { token } });
      return { state: r.state, email: null, organization: null, expiresAt: r.expires_at ?? null };
    } catch (e) {
      // Resets that are not offered make every reset link a dead one.
      if (purpose === 'reset' && e instanceof ApiError && e.status === 404)
        return { state: 'unknown', email: null, organization: null, expiresAt: null };
      return { state: null, email: null, organization: null, expiresAt: null };
    }
  },
  /**
   * The address that starts an SSO sign-in; the browser navigates there. `trust` asks to trust this
   * device, which the gateway honours only for OIDC when the provider asserted a second factor.
   */
  ssoUrl(type: 'oidc' | 'oauth' | 'saml', provider: string, returnTo: string, trust = false): string {
    const query = `return_to=${encodeURIComponent(returnTo)}${trust && type === 'oidc' ? '&trust_device=true' : ''}`;
    return `${API_BASE}/auth/${type}/${encodeURIComponent(provider)}/login?${query}`;
  },
};
