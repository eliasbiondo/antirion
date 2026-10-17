'use client';

import type { ReactNode } from 'react';

import { Checkbox, Combobox, Field, Segmented, Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { cn } from '@/lib/cn';
import { presetLabel, PRESETS } from '@/lib/time';
import {
  DEFAULT_PREFERENCES,
  type Density,
  type PriceUnit,
  type Theme,
  usePreferences,
} from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useSession } from '@/stores/session';

import { hasAccount } from '../describe';
import { LOCALES, useMessages } from '../i18n';
import { Tooltip } from '@/components/ui/tooltip';

const ZONES = [
  'UTC',
  'America/Los_Angeles',
  'America/New_York',
  'America/Sao_Paulo',
  'Europe/London',
  'Europe/Berlin',
  'Asia/Kolkata',
  'Asia/Singapore',
  'Asia/Tokyo',
  'Australia/Sydney',
];

/** The console's cyan first; the rest are spread around the wheel and readable in both themes. */
const ACCENTS = [
  { hue: DEFAULT_PREFERENCES.accentHue, name: 'Cyan' },
  { hue: 150, name: 'Green' },
  { hue: 250, name: 'Blue' },
  { hue: 295, name: 'Violet' },
  { hue: 350, name: 'Pink' },
  { hue: 60, name: 'Amber' },
];

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section aria-label={title} className="flex flex-col gap-4">
      <h3 className="text-body font-semibold text-fg">{title}</h3>
      {children}
    </section>
  );
}

/** One accent swatch; its colour is set through the CSSOM, as the console's CSP requires. */
function AccentSwatch({
  hue,
  name,
  selected,
  onSelect,
}: {
  hue: number;
  name: string;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <Tooltip content={name} describe={false}>
      <button
        type="button"
        role="radio"
        aria-checked={selected}
        aria-label={name}
        tabIndex={selected ? 0 : -1}
        onClick={onSelect}
        className={cn(
          'flex h-7 w-7 items-center justify-center rounded-full border-2 text-white transition-[border-color]',
          selected ? 'border-fg' : 'border-transparent hover:border-line-strong',
        )}
      >
        <span
          ref={(el) => {
            el?.style.setProperty('background', `oklch(0.64 0.14 ${hue})`);
          }}
          className="flex h-5 w-5 items-center justify-center rounded-full"
        >
          {selected ? <Icon name="check" size={11} /> : null}
        </span>
      </button>
    </Tooltip>
  );
}

/** Display preferences: saved to the account for a person, in this browser for an admin token. */
export function PreferencesSection() {
  const prefs = usePreferences();
  const words = useMessages().language;
  const account = useSession((s) => hasAccount(s.me));
  const custom = !ACCENTS.some((a) => a.hue === prefs.accentHue);
  const me = useSession((s) => s.me);
  const active = scopeOrganization(useScope((s) => s.scope));
  const organizations = [
    ...new Set([
      ...Object.keys(me?.permissions ?? {})
        .filter((k) => k.startsWith('organization/'))
        .map((k) => k.slice('organization/'.length)),
      ...(active ? [active] : []),
      ...(prefs.defaultOrganization ? [prefs.defaultOrganization] : []),
    ]),
  ].sort();
  return (
    <div className="flex flex-col gap-7">
      <p className="text-ui text-muted">
        {account
          ? 'Saved to your account and used on all your devices.'
          : 'Saved in this browser only, because admin tokens have no account to keep them.'}
      </p>
      <Group title="Appearance">
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label="Theme">
            <Segmented<Theme>
              label="Theme"
              value={prefs.theme}
              onChange={(theme) => prefs.set({ theme })}
              options={[
                { value: 'light', label: 'Light' },
                { value: 'dark', label: 'Dark' },
                { value: 'system', label: 'System' },
              ]}
            />
          </Field>
          <Field label="Density">
            <Segmented<Density>
              label="Density"
              value={prefs.density}
              onChange={(density) => prefs.set({ density })}
              options={[
                { value: 'compact', label: 'Compact' },
                { value: 'comfortable', label: 'Comfortable' },
              ]}
            />
          </Field>
        </div>
        <Field label="Accent colour">
          <div
            role="radiogroup"
            aria-label="Accent colour"
            className="flex flex-wrap items-center gap-1.5"
            onKeyDown={(e) => {
              if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
              e.preventDefault();
              const i = ACCENTS.findIndex((a) => a.hue === prefs.accentHue);
              const step = e.key === 'ArrowRight' ? 1 : ACCENTS.length - 1;
              const next = ACCENTS[(Math.max(i, 0) + step) % ACCENTS.length];
              if (next) prefs.set({ accentHue: next.hue });
            }}
          >
            {ACCENTS.map((a) => (
              <AccentSwatch
                key={a.hue}
                hue={a.hue}
                name={a.name}
                selected={a.hue === prefs.accentHue}
                onSelect={() => prefs.set({ accentHue: a.hue })}
              />
            ))}
            {custom ? (
              <span className="ml-1 text-caption text-muted">Custom ({prefs.accentHue}°)</span>
            ) : null}
          </div>
        </Field>
      </Group>
      <Group title="Language and time">
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label={words.label} hint={words.hint}>
            <Select
              value={prefs.locale ?? ''}
              onChange={(v) => prefs.set({ locale: v || null })}
              emptyOption={words.browser}
              options={LOCALES.map((l) => ({ value: l.tag, label: l.name }))}
            />
          </Field>
          <Field label="Time zone">
            <Combobox
              value={prefs.timeZone}
              onChange={(v) => prefs.set({ timeZone: v })}
              options={ZONES.map((z) => ({ value: z, label: z.replace(/_/g, ' ') }))}
            />
          </Field>
          <Field label="Open pages with" hint="When a link doesn't name a time range.">
            <Select
              value={prefs.defaultRange ?? ''}
              onChange={(v) => prefs.set({ defaultRange: v || null })}
              emptyOption="Last 6 hours"
              options={PRESETS.map((p) => ({ value: p, label: presetLabel(p) }))}
            />
          </Field>
          <Field label="Week starts on">
            <Select
              value={prefs.weekStart}
              onChange={(v) => prefs.set({ weekStart: v === 'sunday' ? 'sunday' : 'monday' })}
              options={[
                { value: 'monday', label: 'Monday' },
                { value: 'sunday', label: 'Sunday' },
              ]}
            />
          </Field>
        </div>
      </Group>
      <Group title="Data and layout">
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label="Show prices per" hint="Token prices on every screen.">
            <Segmented<PriceUnit>
              label="Show prices per"
              value={prefs.priceUnit}
              onChange={(priceUnit) => prefs.set({ priceUnit })}
              options={[
                { value: '1M', label: '1M tokens' },
                { value: '1K', label: '1K tokens' },
              ]}
            />
          </Field>
          {organizations.length > 1 ? (
            <Field label="Start in organization" hint="When a link doesn't name one.">
              <Combobox
                value={prefs.defaultOrganization ?? ''}
                onChange={(v) => prefs.set({ defaultOrganization: v || null })}
                emptyOption="The last one you used"
                options={organizations.map((o) => ({ value: o, label: o }))}
              />
            </Field>
          ) : null}
          <Field label="Auto-refresh" hint="Pauses while the tab is in the background.">
            <Select
              value={String(prefs.pollSeconds)}
              onChange={(v) => prefs.set({ pollSeconds: Number(v) })}
              options={[
                { value: '5', label: 'Every 5 seconds' },
                { value: '15', label: 'Every 15 seconds' },
                { value: '60', label: 'Every minute' },
                { value: '0', label: 'Off' },
              ]}
            />
          </Field>
        </div>
        <div className="flex flex-col gap-2.5">
          <Checkbox
            label="Shorten large numbers"
            description="Show 12.3K instead of 12,345."
            checked={prefs.compactNumbers}
            onChange={(v) => prefs.set({ compactNumbers: v })}
          />
          <Checkbox
            label="Collapse the sidebar"
            checked={prefs.sidebarCollapsed}
            onChange={(v) => prefs.set({ sidebarCollapsed: v })}
          />
          <Checkbox
            label="Wide summary figures"
            description="Give the figures at the top of a page more room."
            checked={prefs.wideKpis}
            onChange={(v) => prefs.set({ wideKpis: v })}
          />
          <Checkbox
            label="Open the request log's filters"
            description="Start with the filter rail open; f shows or hides it."
            checked={prefs.filterRail}
            onChange={(v) => prefs.set({ filterRail: v })}
          />
          <Checkbox
            label="Show keyboard hints under lists"
            checked={prefs.keyboardHints}
            onChange={(v) => prefs.set({ keyboardHints: v })}
          />
          <Checkbox
            label="Reduce motion"
            checked={prefs.reducedMotion}
            onChange={(v) => prefs.set({ reducedMotion: v })}
          />
        </div>
      </Group>
    </div>
  );
}
