'use client';

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { DatePicker, parseTime, TimeField, toDay } from '@/components/ui/date-picker';
import { Field, Input, Segmented } from '@/components/ui/form';
import { Section } from '@/components/ui/layout';
import { InlineError } from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import { parseDuration } from '@/lib/format';
import { formatDateTime } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';
import { useShell } from '@/stores/shell';
import { useTimeRange } from '@/stores/time-range';
import { notify } from '@/stores/ui';

import { isGatewayAdmin, RESET_SCOPES, type ResetScope, testModeApi } from '../api';

const STEPS = [
  { value: '1m', label: '1 minute' },
  { value: '1h', label: '1 hour' },
  { value: '1d', label: '1 day' },
  { value: '30d', label: '30 days' },
  { value: 'other', label: 'Other' },
] as const;
type Step = (typeof STEPS)[number]['value'];

function refreshPage() {
  useTimeRange.getState().refresh();
}

function Clock() {
  const timeZone = usePreferences((s) => s.timeZone);
  const [now, setNow] = useState<string | null>(null);
  const [step, setStep] = useState<Step>('1h');
  const [other, setOther] = useState('');
  const [day, setDay] = useState('');
  const [time, setTime] = useState('00:00');
  const [busy, setBusy] = useState<'advance' | 'set' | null>(null);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    let live = true;
    testModeApi.clock().then(
      (c) => {
        if (live) setNow(c.now);
      },
      (e: unknown) => {
        if (live) setError(e);
      },
    );
    return () => {
      live = false;
    };
  }, []);

  const by = step === 'other' ? other.trim() : step;
  const byValid = parseDuration(by) !== null && (parseDuration(by) ?? 0) > 0;
  const t = parseTime(time);
  const target =
    day && t ? `${day}T${String(t.h).padStart(2, '0')}:${String(t.m).padStart(2, '0')}:00Z` : null;
  const targetError =
    target && now && Date.parse(target) <= Date.parse(now) ? 'The clock only moves forward.' : null;

  async function run(kind: 'advance' | 'set') {
    setBusy(kind);
    setError(null);
    try {
      const c = kind === 'advance' ? await testModeApi.advance(by) : await testModeApi.setClock(target ?? '');
      setNow(c.now);
      notify('success', 'Clock moved', `The gateway's time is now ${formatDateTime(c.now, timeZone)}.`);
      refreshPage();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-baseline gap-2">
        <h4 className="text-ui font-semibold text-fg">Clock</h4>
        <span className="mono text-ui text-fg">{now ? formatDateTime(now, timeZone) : '—'}</span>
        <span className="text-caption text-muted">
          the gateway&apos;s time; budgets, limits and windows follow it
        </span>
      </div>
      <InlineError error={error} />
      <div className="flex flex-wrap items-end gap-2">
        <Segmented<Step> label="Advance by" value={step} onChange={setStep} options={[...STEPS]} />
        {step === 'other' ? (
          <div className="w-28">
            <Input
              mono
              aria-label="Duration"
              placeholder="90m"
              value={other}
              onChange={(e) => setOther(e.target.value)}
            />
          </div>
        ) : null}
        <Button
          icon="arrowRight"
          busy={busy === 'advance'}
          disabled={!byValid}
          onClick={() => void run('advance')}
        >
          Advance
        </Button>
      </div>
      <div className="flex flex-wrap items-end gap-2">
        <Field label="Or set it to (UTC)" error={targetError ?? undefined}>
          <DatePicker
            value={day}
            onChange={setDay}
            min={now ? toDay(new Date(now)) : undefined}
            placeholder="Pick a day"
            aria-label="Day"
          />
        </Field>
        <TimeField value={time} onChange={setTime} aria-label="Time" className="w-24" />
        <Button
          busy={busy === 'set'}
          disabled={!target || Boolean(targetError)}
          onClick={() => void run('set')}
        >
          Set clock
        </Button>
      </div>
    </div>
  );
}

function Reset() {
  const [scopes, setScopes] = useState<ResetScope[]>(RESET_SCOPES.map((s) => s.value));
  const [confirming, setConfirming] = useState(false);
  const all = scopes.length === RESET_SCOPES.length;
  const labels = RESET_SCOPES.filter((s) => scopes.includes(s.value)).map((s) => s.label.toLowerCase());
  return (
    <div className="flex flex-col gap-3">
      <h4 className="text-ui font-semibold text-fg">Reset state</h4>
      <div role="group" aria-label="What to clear" className="grid gap-x-6 gap-y-2 sm:grid-cols-2">
        {RESET_SCOPES.map((s) => (
          <Checkbox
            key={s.value}
            label={s.label}
            checked={scopes.includes(s.value)}
            onChange={(on) => setScopes(on ? [...scopes, s.value] : scopes.filter((x) => x !== s.value))}
          />
        ))}
      </div>
      <div>
        <Button
          variant="danger"
          icon="refresh"
          disabled={scopes.length === 0}
          onClick={() => setConfirming(true)}
        >
          Reset {all ? 'everything' : 'selected'}
        </Button>
      </div>
      <ConfirmDialog
        open={confirming}
        onClose={() => setConfirming(false)}
        withReason={false}
        title={all ? 'Reset all runtime state?' : 'Reset the selected state?'}
        confirmLabel="Reset state"
        effect={`The gateway clears ${labels.join(', ')}. Its configuration, keys and accounts are kept.`}
        onConfirm={async () => {
          await testModeApi.reset(scopes);
          notify('success', 'State reset', `Cleared ${labels.join(', ')}.`);
          refreshPage();
        }}
      />
    </div>
  );
}

function Seed() {
  const [seed, setSeed] = useState('');
  const [busy, setBusy] = useState(false);
  const value = /^[0-9]{1,20}$/.test(seed.trim()) ? Number(seed.trim()) : null;
  return (
    <div className="flex flex-col gap-3">
      <div>
        <h4 className="text-ui font-semibold text-fg">Randomness</h4>
        <p className="text-caption text-muted">
          Weighted splits, sampling and jitter make the same choices again for the same seed.
        </p>
      </div>
      <div className="flex flex-wrap items-end gap-2">
        <div className="w-56">
          <Input
            mono
            inputMode="numeric"
            aria-label="Seed"
            placeholder="42"
            value={seed}
            onChange={(e) => setSeed(e.target.value)}
          />
        </div>
        <Button
          busy={busy}
          disabled={value === null || !Number.isSafeInteger(value)}
          onClick={() => {
            if (value === null) return;
            setBusy(true);
            testModeApi.seed(value).then(
              () => {
                notify('success', 'Seed set', `Random choices now follow seed ${value}.`);
                setBusy(false);
              },
              (e: unknown) => {
                notify('error', "Couldn't set the seed", describeError(e));
                setBusy(false);
              },
            );
          }}
        >
          Set seed
        </Button>
      </div>
    </div>
  );
}

/**
 * Test mode's tools on the Status page: the gateway clock, a reset of runtime state and the random
 * seed. Shown only while the gateway runs with `--test-mode`, to gateway admins.
 */
export function TestModePanel() {
  const testMode = useShell((s) => s.status?.test_mode === true);
  const me = useSession((s) => s.me);
  if (!testMode || !isGatewayAdmin(me)) return null;
  return (
    <div role="region" aria-label="Test-mode tools">
      <Section
        title="Test mode"
        subtitle="This gateway was started with --test-mode, for tests only. Every action here is recorded in the audit log."
      >
        <div className="flex flex-col gap-6">
          <Clock />
          <Reset />
          <Seed />
        </div>
      </Section>
    </div>
  );
}
