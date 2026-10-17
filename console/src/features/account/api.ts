import { api } from '@/lib/api/client';
import type { MfaFactor, PersonalToken, PersonalTokenCreated, Session } from '@/lib/api/types';

export interface Factors {
  items: MfaFactor[];
  recoveryCodesRemaining: number;
}

export const accountApi = {
  sessions: async (): Promise<Session[]> => (await api.get('/auth/sessions')).items,
  revokeSession: (id: string) => api.delete('/auth/sessions/{session_id}', { path: { session_id: id } }),
  revokeOthers: () => api.post('/auth/sessions:revoke_others'),
  tokens: async (): Promise<PersonalToken[]> => (await api.get('/auth/me/tokens')).items,
  createToken: (body: {
    description?: string;
    expires_at: string;
    access: 'full' | 'read_only';
  }): Promise<PersonalTokenCreated> => api.post('/auth/me/tokens', { body }),
  deleteToken: (kid: string) => api.delete('/auth/me/tokens/{kid}', { path: { kid } }),
  factors: async (): Promise<Factors> => {
    const list = await api.get('/auth/me/mfa');
    return { items: list.items, recoveryCodesRemaining: list.recovery_codes_remaining };
  },
  /** Starts adding an authenticator app to a signed-in account: a secret and its `otpauth://` URI. */
  enrollTotp: () => api.post('/auth/me/mfa/totp'),
  confirmTotp: (body: { factor_id: string; totp: string; name?: string }) =>
    api.post('/auth/me/mfa/totp:confirm', { body }),
  /** Starts adding a security key to a signed-in account: the browser's registration options. */
  enrollWebauthn: () => api.post('/auth/me/mfa/webauthn'),
  confirmWebauthn: (body: { factor_id: string; webauthn: Record<string, unknown>; name?: string }) =>
    api.post('/auth/me/mfa/webauthn:confirm', { body }),
  deleteFactor: (id: string) => api.delete('/auth/me/mfa/{factor_id}', { path: { factor_id: id } }),
  regenerateRecoveryCodes: (body: { factor_id: string; totp: string }) =>
    api.post('/auth/me/mfa:regenerate_recovery_codes', { body }),
  updateProfile: (body: { display_name?: string; title?: string; email?: string }) =>
    api.patch('/auth/me', { body }),
  changePassword: (body: {
    current_password: string;
    new_password: string;
    totp?: string;
    recovery_code?: string;
  }) => api.post('/auth/me:change_password', { body }),
};
