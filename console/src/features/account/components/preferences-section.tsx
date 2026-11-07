'use client';

import type { ReactNode } from 'react';

import { Checkbox, Combobox, Field, Segmented, Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { cn } from '@/lib/cn';
import { LOCALES, matchLocale } from '@/lib/locale';
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
import { useMessages } from '../i18n';
import { Tooltip } from '@/components/ui/tooltip';
import { useTranslate } from '@/lib/i18n';

const ZONES = [
  'UTC',
  'America/Los_Angeles',
  'America/New_York',
  'America/Sao_Paulo',
  'Europe/London',
  'Europe/Berlin',
  'Asia/Kolkata',
  'Asia/Tehran',
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
  const translate = useTranslate();
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
          ? translate('Saved to your account and used on all your devices.')
          : translate('Saved in this browser only, because admin tokens have no account to keep them.')}
      </p>
      <Group title={translate('Appearance')}>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label={translate('Theme')}>
            <Segmented<Theme>
              label={translate('Theme')}
              value={prefs.theme}
              onChange={(theme) => prefs.set({ theme })}
              options={[
                { value: 'light', label: translate('Light') },
                { value: 'dark', label: translate('Dark') },
                { value: 'system', label: translate('System') },
              ]}
            />
          </Field>
          <Field label={translate('Density')}>
            <Segmented<Density>
              label={translate('Density')}
              value={prefs.density}
              onChange={(density) => prefs.set({ density })}
              options={[
                { value: 'compact', label: translate('Compact') },
                { value: 'comfortable', label: translate('Comfortable') },
              ]}
            />
          </Field>
        </div>
        <Field label={translate('Accent colour')}>
          <div
            role="radiogroup"
            aria-label={translate('Accent colour')}
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
              <span className="ml-1 text-caption text-muted">
                {translate('Custom (')}
                {prefs.accentHue}°)
              </span>
            ) : null}
          </div>
        </Field>
      </Group>
      <Group title={translate('Language and time')}>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label={words.label} hint={words.hint}>
            <Select
              value={matchLocale(prefs.locale)?.tag ?? ''}
              onChange={(v) => prefs.set({ locale: v || null })}
              emptyOption={words.browser}
              options={LOCALES.map((l) => ({ value: l.tag, label: l.name }))}
            />
          </Field>
          <Field label={translate('Time zone')}>
            <Combobox
              value={prefs.timeZone}
              onChange={(v) => prefs.set({ timeZone: v })}
              options={ZONES.map((z) => ({ value: z, label: z.replace(/_/g, ' ') }))}
            />
          </Field>
          <Field
            label={translate('Open pages with')}
            hint={translate("When a link doesn't name a time range.")}
          >
            <Select
              value={prefs.defaultRange ?? ''}
              onChange={(v) => prefs.set({ defaultRange: v || null })}
              emptyOption={translate('Last 6 hours')}
              options={PRESETS.map((p) => ({ value: p, label: translate(presetLabel(p)) }))}
            />
          </Field>
          <Field label={translate('Week starts on')}>
            <Select
              value={prefs.weekStart}
              onChange={(v) => prefs.set({ weekStart: v === 'sunday' ? 'sunday' : 'monday' })}
              options={[
                { value: 'monday', label: translate('Monday') },
                { value: 'sunday', label: translate('Sunday') },
              ]}
            />
          </Field>
        </div>
      </Group>
      <Group title={translate('Data and layout')}>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label={translate('Show prices per')} hint={translate('Token prices on every screen.')}>
            <Segmented<PriceUnit>
              label={translate('Show prices per')}
              value={prefs.priceUnit}
              onChange={(priceUnit) => prefs.set({ priceUnit })}
              options={[
                { value: '1M', label: translate('1M tokens') },
                { value: '1K', label: translate('1K tokens') },
              ]}
            />
          </Field>
          {organizations.length > 1 ? (
            <Field
              label={translate('Start in organization')}
              hint={translate("When a link doesn't name one.")}
            >
              <Combobox
                value={prefs.defaultOrganization ?? ''}
                onChange={(v) => prefs.set({ defaultOrganization: v || null })}
                emptyOption={translate('The last one you used')}
                options={organizations.map((o) => ({ value: o, label: o }))}
              />
            </Field>
          ) : null}
          <Field
            label={translate('Auto-refresh')}
            hint={translate('Pauses while the tab is in the background.')}
          >
            <Select
              value={String(prefs.pollSeconds)}
              onChange={(v) => prefs.set({ pollSeconds: Number(v) })}
              options={[
                { value: '5', label: translate('Every 5 seconds') },
                { value: '15', label: translate('Every 15 seconds') },
                { value: '60', label: translate('Every minute') },
                { value: '0', label: translate('Off') },
              ]}
            />
          </Field>
        </div>
        <div className="flex flex-col gap-2.5">
          <Checkbox
            label={translate('Shorten large numbers')}
            description={translate('Show 12.3K instead of 12,345.')}
            checked={prefs.compactNumbers}
            onChange={(v) => prefs.set({ compactNumbers: v })}
          />
          <Checkbox
            label={translate('Collapse the sidebar')}
            checked={prefs.sidebarCollapsed}
            onChange={(v) => prefs.set({ sidebarCollapsed: v })}
          />
          <Checkbox
            label={translate('Wide summary figures')}
            description={translate('Give the figures at the top of a page more room.')}
            checked={prefs.wideKpis}
            onChange={(v) => prefs.set({ wideKpis: v })}
          />
          <Checkbox
            label={translate("Open the request log's filters")}
            description={translate('Start with the filter rail open; f shows or hides it.')}
            checked={prefs.filterRail}
            onChange={(v) => prefs.set({ filterRail: v })}
          />
          <Checkbox
            label={translate('Show keyboard hints under lists')}
            checked={prefs.keyboardHints}
            onChange={(v) => prefs.set({ keyboardHints: v })}
          />
          <Checkbox
            label={translate('Reduce motion')}
            checked={prefs.reducedMotion}
            onChange={(v) => prefs.set({ reducedMotion: v })}
          />
        </div>
      </Group>
    </div>
  );
}
