import { create } from 'zustand';

import { ApiError } from '@/lib/api/client';
import type { LoginOptions } from '@/lib/api/types';
import { useSession } from '@/stores/session';
import { LAST_ORGANIZATION_KEY } from '@/stores/scope';

import { webauthnCreate, webauthnGet } from '@/lib/webauthn';

import { authApi } from './api';

/** The assertion options the gateway adds to a step when the account has a security key. */
function webauthnOptions(step: object): Record<string, unknown> | null {
  const options = (step as { webauthn_options?: unknown }).webauthn_options;
  return typeof options === 'object' && options !== null ? (options as Record<string, unknown>) : null;
}

/**
 * What an emailed link turned out to be: `invalid` is a link the gateway does not know (replaced,
 * cancelled or mistyped); `unknown` means the check could not tell, so the form shows.
 */
export type LinkStatus = 'checking' | 'valid' | 'expired' | 'used' | 'invalid' | 'missing' | 'unknown';

/** What a valid link is for: the invited address and organization, and when the link stops working. */
export interface LinkFacts {
  email: string | null;
  organization: string | null;
  expiresAt: string | null;
}

export type LoginStep =
  | { kind: 'start' }
  /** The sign-in methods even while the gateway has no account yet (the start step is then the token). */
  | { kind: 'methods' }
  | { kind: 'token' }
  | { kind: 'cli' }
  | { kind: 'organization' }
  | { kind: 'mfa'; challenge: string; webauthn: Record<string, unknown> | null }
  | {
      kind: 'enroll';
      challenge: string;
      factorId: string | null;
      secret: string | null;
      uri: string | null;
    }
  | { kind: 'recovery-codes'; codes: string[] }
  | { kind: 'forgot' }
  | { kind: 'forgot-sent' }
  | {
      kind: 'reset';
      token: string;
      purpose: 'reset' | 'invitation';
      link: LinkStatus;
      facts?: LinkFacts;
    }
  | { kind: 'done' };

/** Where a password-setting flow started, for the step counter ("Step 2 of 3"). */
export type Flow = 'sign-in' | 'invitation';

interface LoginState {
  options: LoginOptions | null;
  organization: string;
  /** The email typed on the first step, shown on the second-factor step and kept for "Forgot password?". */
  email: string;
  flow: Flow;
  step: LoginStep;
  busy: boolean;
  /** One message for a failed step; for sign-in it never says whether the account exists. */
  error: string | null;
  /** A problem with the new password that belongs under its field (the gateway's policy). */
  passwordError: string | null;
  trustDevice: boolean;
  loadOptions: (organization?: string) => Promise<void>;
  setOrganization: (organization: string) => void;
  /** Sets the organization, reloads the methods it offers and returns to the first step. */
  chooseOrganization: (organization: string) => Promise<void>;
  setEmail: (email: string) => void;
  setTrustDevice: (trust: boolean) => void;
  go: (step: LoginStep) => void;
  signInWithToken: (token: string) => Promise<boolean>;
  signInLocal: (email: string, password: string) => Promise<boolean>;
  verifyMfa: (code: { totp?: string; recoveryCode?: string; securityKey?: boolean }) => Promise<boolean>;
  enrollSecurityKey: () => Promise<void>;
  startEnrollment: () => Promise<void>;
  confirmEnrollment: (totp: string) => Promise<boolean>;
  finishEnrollment: () => void;
  requestReset: (email: string) => Promise<void>;
  checkLink: () => Promise<void>;
  confirmReset: (password: string) => Promise<void>;
}

export const MESSAGES = {
  password: "That email and password don't match. Try again, or reset your password.",
  passwordNoReset: "That email and password don't match. Try again.",
  token: "That token didn't work. Check that you copied all of it and try again.",
  ssoRequired: 'Your organization signs in with single sign-on. Use its button above instead of a password.',
  throttled: 'Too many attempts. Wait a few minutes, then try again.',
  code: "That code didn't work. Codes change every 30 seconds; enter the newest one.",
  recoveryCode: "That recovery code didn't work. Check it, or try another one; each works only once.",
  securityKey: "Your security key wasn't accepted. Try again, or enter a code instead.",
  enrolStart: "We couldn't start the setup. Go back to sign in and try again.",
  enrolCode:
    "That code didn't work. Make sure your phone's clock is set automatically, then enter the newest code.",
  enrolKey: "Your security key wasn't registered. Try again, or use an authenticator app.",
} as const;

/** Plain words for a refused sign-in. A 401 stays generic so it never reveals which part was wrong. */
function signInMessage(e: unknown, fallback: string): string {
  if (!(e instanceof ApiError)) return fallback;
  if (e.status === 0) return "Can't reach the gateway. Check your connection and try again.";
  if (e.status === 429) return MESSAGES.throttled;
  if (e.status === 403 && e.details.some((d) => d.code === 'local_login_disabled'))
    return MESSAGES.ssoRequired;
  return fallback;
}

/** The gateway's password-policy failures (detail code `password_policy`) as instructions. */
const POLICY: Record<string, string> = {
  min_length: 'Use a longer password; this gateway asks for more characters.',
  max_length: 'Use a shorter password.',
  deny_common: 'This password is too common. Choose one that is harder to guess.',
  email: "Don't use your email address in your password.",
  history: "Choose a password you haven't used recently.",
};

export function policyMessage(e: ApiError): string | null {
  const rule = e.details.find((d) => d.code === 'password_policy');
  if (!rule) return null;
  return POLICY[rule.message] ?? POLICY[rule.path] ?? 'Choose a different password.';
}

function rememberOrganization(org: string) {
  try {
    if (org) localStorage.setItem(LAST_ORGANIZATION_KEY, org);
    else localStorage.removeItem(LAST_ORGANIZATION_KEY);
  } catch {
    return;
  }
}

/** The sign-in flow: methods from `login_options`, then token, password and second factor. */
export const useLogin = create<LoginState>()((set, get) => ({
  options: null,
  organization: '',
  email: '',
  flow: 'sign-in',
  step: { kind: 'start' },
  busy: false,
  error: null,
  passwordError: null,
  trustDevice: false,

  async loadOptions(organization) {
    const org = organization ?? get().organization;
    const options = await authApi.loginOptions(org || undefined);
    set({ options });
  },

  setOrganization(organization) {
    set({ organization });
  },

  async chooseOrganization(organization) {
    const org = organization.trim().toLowerCase();
    set({ organization: org, busy: true });
    try {
      await get().loadOptions(org);
      rememberOrganization(org);
    } finally {
      set({ busy: false, step: { kind: 'start' }, error: null });
    }
  },

  setEmail(email) {
    set({ email });
  },

  setTrustDevice(trustDevice) {
    set({ trustDevice });
  },

  go(step) {
    set({
      step,
      error: null,
      passwordError: null,
      ...(step.kind === 'start' || step.kind === 'methods' || step.kind === 'token'
        ? { flow: 'sign-in' as const }
        : {}),
    });
  },

  async signInWithToken(token) {
    set({ busy: true, error: null });
    try {
      await useSession.getState().signInWithToken(token);
      rememberOrganization(get().organization);
      set({ step: { kind: 'done' } });
      return true;
    } catch (e) {
      set({ error: signInMessage(e, MESSAGES.token) });
      return false;
    } finally {
      set({ busy: false });
    }
  },

  async signInLocal(email, password) {
    set({ busy: true, error: null, email, flow: 'sign-in' });
    try {
      const org = get().organization;
      const step = await authApi.localLogin({ email, password, ...(org ? { organization: org } : {}) });
      rememberOrganization(org);
      if (step.next === 'enroll_mfa') {
        set({
          step: {
            kind: 'enroll',
            challenge: step.challenge,
            factorId: null,
            secret: null,
            uri: null,
          },
        });
      } else {
        set({ step: { kind: 'mfa', challenge: step.challenge, webauthn: webauthnOptions(step) } });
      }
      return true;
    } catch (e) {
      const canReset = get().options?.password_reset === true;
      set({ error: signInMessage(e, canReset ? MESSAGES.password : MESSAGES.passwordNoReset) });
      return false;
    } finally {
      set({ busy: false });
    }
  },

  async verifyMfa({ totp, recoveryCode, securityKey }) {
    const step = get().step;
    if (step.kind !== 'mfa') return false;
    set({ busy: true, error: null });
    try {
      const trust_device = get().trustDevice;
      const body =
        securityKey && step.webauthn
          ? { challenge: step.challenge, webauthn: await webauthnGet(step.webauthn), trust_device }
          : totp
            ? { challenge: step.challenge, totp, trust_device }
            : { challenge: step.challenge, recovery_code: recoveryCode ?? '', trust_device };
      const me = await authApi.localMfa(body);
      useSession.getState().adopt(me);
      set({ step: { kind: 'done' } });
      return true;
    } catch (e) {
      const fallback = securityKey ? MESSAGES.securityKey : totp ? MESSAGES.code : MESSAGES.recoveryCode;
      set({ error: signInMessage(e, fallback) });
      return false;
    } finally {
      set({ busy: false });
    }
  },

  async startEnrollment() {
    const step = get().step;
    if (step.kind !== 'enroll') return;
    set({ busy: true, error: null });
    try {
      const enrolment = await authApi.enrollTotp(step.challenge);
      set({
        step: {
          ...step,
          factorId: enrolment.factor_id,
          secret: enrolment.secret ?? null,
          uri: enrolment.otpauth_uri ?? null,
        },
      });
    } catch {
      set({ error: MESSAGES.enrolStart });
    } finally {
      set({ busy: false });
    }
  },

  async confirmEnrollment(totp) {
    const step = get().step;
    if (step.kind !== 'enroll' || !step.factorId) return false;
    set({ busy: true, error: null });
    try {
      const codes = await authApi.confirmTotp({
        challenge: step.challenge,
        factor_id: step.factorId,
        totp,
      });
      useSession.getState().adopt(await authApi.me());
      set({ step: { kind: 'recovery-codes', codes: codes.recovery_codes } });
      return true;
    } catch (e) {
      set({ error: signInMessage(e, MESSAGES.enrolCode) });
      return false;
    } finally {
      set({ busy: false });
    }
  },

  async enrollSecurityKey() {
    const step = get().step;
    if (step.kind !== 'enroll') return;
    set({ busy: true, error: null });
    try {
      const enrolment = await authApi.enrollWebauthn(step.challenge);
      if (!enrolment.webauthn_options) throw new Error('no options');
      const credential = await webauthnCreate(enrolment.webauthn_options);
      const codes = await authApi.confirmWebauthn({
        challenge: step.challenge,
        factor_id: enrolment.factor_id,
        webauthn: credential,
      });
      useSession.getState().adopt(await authApi.me());
      set({ step: { kind: 'recovery-codes', codes: codes.recovery_codes } });
    } catch {
      set({ error: MESSAGES.enrolKey });
    } finally {
      set({ busy: false });
    }
  },

  finishEnrollment() {
    set({ step: { kind: 'done' } });
  },

  async requestReset(email) {
    set({ busy: true, error: null, email });
    try {
      await authApi.requestReset(email);
    } catch {
      // The answer is the same whether or not the address has an account.
    } finally {
      set({ busy: false, step: { kind: 'forgot-sent' } });
    }
  },

  async checkLink() {
    const step = get().step;
    if (step.kind !== 'reset' || step.link !== 'checking') return;
    const check = await authApi.checkLink(step.purpose, step.token);
    const link: LinkStatus =
      check.state === null
        ? 'unknown'
        : check.state === 'valid'
          ? 'valid'
          : check.state === 'unknown'
            ? 'invalid'
            : check.state;
    const facts = { email: check.email, organization: check.organization, expiresAt: check.expiresAt };
    const current = get().step;
    if (current.kind === 'reset' && current.token === step.token) set({ step: { ...current, link, facts } });
  },

  async confirmReset(password) {
    const step = get().step;
    if (step.kind !== 'reset') return;
    set({ busy: true, error: null, passwordError: null });
    try {
      const next =
        step.purpose === 'invitation'
          ? await authApi.acceptInvitation(step.token, password)
          : await authApi.confirmReset(step.token, password);
      set({
        email: step.facts?.email ?? get().email,
        flow: step.purpose === 'invitation' ? 'invitation' : 'sign-in',
        step:
          next.next === 'enroll_mfa'
            ? {
                kind: 'enroll',
                challenge: next.challenge,
                factorId: null,
                secret: null,
                uri: null,
              }
            : { kind: 'mfa', challenge: next.challenge, webauthn: webauthnOptions(next) },
      });
    } catch (e) {
      const policy = e instanceof ApiError ? policyMessage(e) : null;
      if (policy) set({ passwordError: policy });
      else if (e instanceof ApiError && e.code === 'invalid_parameter')
        set({ step: { ...step, link: 'invalid' } });
      else set({ error: signInMessage(e, 'Something went wrong. Try again.') });
    } finally {
      set({ busy: false });
    }
  },
}));

/** The days a trusted browser skips the second step, while the gateway offers trusted devices. */
export function useTrustDays(): number | null {
  return useLogin((s) => {
    // A gateway older than these facts leaves them out.
    const trusted = s.options?.trusted_device;
    return trusted?.enabled ? (trusted.max_age_days ?? null) : null;
  });
}
