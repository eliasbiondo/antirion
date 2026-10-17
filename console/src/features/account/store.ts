import { create } from 'zustand';

import type { PersonalToken, Session } from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';
import { useSession } from '@/stores/session';

import { accountApi, type Factors } from './api';

interface AccountState {
  sessions: Slot<Session[]>;
  tokens: Slot<PersonalToken[]>;
  factors: Slot<Factors>;
  loadSessions: () => Promise<void>;
  loadTokens: () => Promise<void>;
  loadFactors: () => Promise<void>;
  revokeSession: (id: string) => Promise<void>;
  revokeOthers: () => Promise<void>;
  /** Returns the new token's value, to be shown once and dropped. */
  createToken: (input: {
    description: string;
    days: number;
    access: 'full' | 'read_only';
  }) => Promise<string>;
  deleteToken: (kid: string) => Promise<void>;
  deleteFactor: (id: string) => Promise<void>;
  updateProfile: (input: { display_name: string; title: string }) => Promise<void>;
  /** Asks for a new email; true when it took effect at once, false when a link was mailed to it. */
  changeEmail: (email: string) => Promise<boolean>;
  /** `code` is a 6-digit authenticator code or a recovery code; the gateway asks for one of them. */
  changePassword: (current: string, next: string, code: string) => Promise<void>;
}

/** The signed-in user's own account: sessions, personal tokens, second factors, profile. */
export const useAccount = create<AccountState>()((set, get) => ({
  sessions: emptySlot(),
  tokens: emptySlot(),
  factors: emptySlot(),

  loadSessions: () => loadSlot(set, get, 'sessions', accountApi.sessions),
  loadTokens: () => loadSlot(set, get, 'tokens', accountApi.tokens),
  loadFactors: () => loadSlot(set, get, 'factors', accountApi.factors),

  async revokeSession(id) {
    await accountApi.revokeSession(id);
    await get().loadSessions();
  },
  async revokeOthers() {
    await accountApi.revokeOthers();
    await get().loadSessions();
  },
  async createToken({ description, days, access }) {
    const expires = new Date(Date.now() + days * 86_400_000).toISOString();
    const created = await accountApi.createToken({ description, expires_at: expires, access });
    const { token, ...listed } = created;
    set((s) => ({ tokens: { ...s.tokens, data: [...(s.tokens.data ?? []), listed] } }));
    return token;
  },
  async deleteToken(kid) {
    await accountApi.deleteToken(kid);
    set((s) => ({ tokens: { ...s.tokens, data: (s.tokens.data ?? []).filter((t) => t.kid !== kid) } }));
  },
  async deleteFactor(id) {
    await accountApi.deleteFactor(id);
    await get().loadFactors();
  },
  async updateProfile(input) {
    const me = await accountApi.updateProfile(input);
    useSession.getState().adopt(me);
  },
  async changeEmail(email) {
    const me = await accountApi.updateProfile({ email });
    useSession.getState().adopt(me);
    return me.user?.email?.toLowerCase() === email.toLowerCase();
  },
  async changePassword(current, next, code) {
    const value = code.replace(/[\s-]/g, '');
    await accountApi.changePassword({
      current_password: current,
      new_password: next,
      ...(/^\d{6}$/.test(value) ? { totp: value } : { recovery_code: code.trim() }),
    });
  },
}));
