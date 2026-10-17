import { duration } from './policy';
import type { RuleSpec } from './api';

export type TriggerEvent = RuleSpec['trigger']['event'];
export type DropKind = RuleSpec['action']['drop'];

export const TRIGGERS: { value: TriggerEvent; label: string; sentence: string }[] = [
  { value: 'model_disabled', label: 'A model is turned off', sentence: 'When a model is turned off' },
  { value: 'deployment_changed', label: 'A deployment changes', sentence: 'When a deployment changes' },
  { value: 'route_changed', label: 'A route changes', sentence: 'When a route changes' },
  { value: 'schedule', label: 'On a schedule', sentence: 'On a schedule' },
  {
    value: 'metadata_changed',
    label: 'A request metadata value changes',
    sentence: 'When a request metadata value changes',
  },
  { value: 'manual', label: 'Only when run by hand', sentence: 'When run by hand' },
];

export const DROPS: { value: DropKind; label: string }[] = [
  { value: 'model', label: 'A model’s responses' },
  { value: 'route', label: 'A route’s responses' },
  { value: 'deployment', label: 'A deployment’s responses' },
  { value: 'key', label: 'An API key’s responses' },
  { value: 'end_user', label: 'An end user’s responses' },
  { value: 'tag', label: 'Responses with a tag' },
  { value: 'metadata', label: 'Responses with a metadata value' },
  { value: 'older_than', label: 'Responses older than' },
];

/** The trigger that names the object a drop of this kind can take its value from. */
const TRIGGER_NAMES: Partial<Record<DropKind, TriggerEvent>> = {
  model: 'model_disabled',
  deployment: 'deployment_changed',
  route: 'route_changed',
};

/** Whether the drop takes its value from the trigger when `value` is left empty. */
export function valueFromTrigger(event: TriggerEvent, drop: DropKind): boolean {
  return TRIGGER_NAMES[drop] === event || (drop === 'metadata' && event === 'metadata_changed');
}

export const SCHEDULES = [
  { value: '0 3 * * *', label: 'Every night at 03:00 UTC' },
  { value: '0 * * * *', label: 'Every hour' },
  { value: '0 0 * * 1', label: 'Every Monday at 00:00 UTC' },
  { value: '*/15 * * * *', label: 'Every 15 minutes' },
];

/** A cron expression in words when it is a common one, else the expression. */
export function describeSchedule(cron: string): string {
  const preset = SCHEDULES.find((s) => s.value === cron.trim());
  if (preset) return preset.label.replace(/^Every/, 'every');
  const m = /^(\d{1,2}) (\d{1,2}) \* \* \*$/.exec(cron.trim());
  if (m) return `every day at ${m[2]?.padStart(2, '0')}:${m[1]?.padStart(2, '0')} UTC`;
  return `on the schedule ${cron.trim()} (UTC)`;
}

const FIELD = /^(\*|\d+(-\d+)?(\/\d+)?|\*\/\d+)(,(\*|\d+(-\d+)?(\/\d+)?|\*\/\d+))*$/;

/** Whether a five-field cron expression has the shape the gateway accepts. */
export function validCron(cron: string): boolean {
  const parts = cron.trim().split(/\s+/);
  return parts.length === 5 && parts.every((p) => FIELD.test(p));
}

/** "When a model is turned off, clear that model's responses." */
export function describeRule(spec: RuleSpec): string {
  const { trigger, action } = spec;
  const when =
    trigger.event === 'schedule' && trigger.schedule
      ? capitalizeFirst(describeSchedule(trigger.schedule))
      : trigger.event === 'metadata_changed' && trigger.metadata_key
        ? `When the request metadata ${trigger.metadata_key} changes`
        : (TRIGGERS.find((t) => t.value === trigger.event)?.sentence ?? trigger.event);
  let what: string;
  if (action.drop === 'older_than') what = `clear responses older than ${duration(action.older_than)}`;
  else if (action.value) {
    const noun =
      action.drop === 'end_user' ? 'end user' : action.drop === 'metadata' ? 'metadata' : action.drop;
    what = `clear the responses of ${noun} ${action.value}`;
  } else if (action.drop === 'metadata') what = `clear the responses recorded with the previous value`;
  else what = `clear that ${action.drop.replace('_', ' ')}’s responses`;
  return `${when}, ${what}.`;
}

function capitalizeFirst(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export interface RuleDraft {
  name: string;
  description: string;
  enabled: boolean;
  event: TriggerEvent;
  schedule: string;
  metadataKey: string;
  drop: DropKind;
  value: string;
  olderThan: string;
}

export const PRESETS: {
  id: string;
  label: string;
  description: string;
  draft: Omit<RuleDraft, 'name'> & { name: string };
}[] = [
  {
    id: 'model-off',
    label: 'Clear a model when it’s turned off',
    description: 'Its cached responses go as soon as the model is disabled.',
    draft: {
      name: 'clear-on-model-off',
      description: 'Clear a model’s cached responses when it is turned off.',
      enabled: true,
      event: 'model_disabled',
      schedule: '',
      metadataKey: '',
      drop: 'model',
      value: '',
      olderThan: '',
    },
  },
  {
    id: 'nightly',
    label: 'Nightly refresh',
    description: 'Every night, drop the oldest responses; the next request fetches a fresh one.',
    draft: {
      name: 'nightly-refresh',
      description: 'Every night, drop responses cached more than 12 hours ago.',
      enabled: true,
      event: 'schedule',
      schedule: '0 3 * * *',
      metadataKey: '',
      drop: 'older_than',
      value: '',
      olderThan: '12h',
    },
  },
];

export function emptyDraft(): RuleDraft {
  return {
    name: '',
    description: '',
    enabled: true,
    event: 'manual',
    schedule: '',
    metadataKey: '',
    drop: 'older_than',
    value: '',
    olderThan: '1d',
  };
}

export function draftOf(name: string, spec: RuleSpec): RuleDraft {
  return {
    name,
    description: spec.description ?? '',
    enabled: spec.enabled !== false,
    event: spec.trigger.event,
    schedule: spec.trigger.schedule ?? '',
    metadataKey: spec.trigger.metadata_key ?? '',
    drop: spec.action.drop,
    value: spec.action.value ?? '',
    olderThan: spec.action.older_than ?? '',
  };
}

/** Field errors of a draft, by field, in the words the form shows. */
export function draftErrors(d: RuleDraft, creating: boolean): Partial<Record<keyof RuleDraft, string>> {
  const e: Partial<Record<keyof RuleDraft, string>> = {};
  if (creating && !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(d.name))
    e.name = 'Start with a letter or digit; then letters, digits, dots, dashes or underscores';
  if (d.description.length > 500) e.description = 'Keep it under 500 characters';
  if (d.event === 'schedule' && !validCron(d.schedule))
    e.schedule = 'Enter five cron fields in UTC, such as 0 3 * * *';
  if (d.event === 'metadata_changed' && !/^.{1,64}$/.test(d.metadataKey.trim()))
    e.metadataKey = 'Name the metadata key to watch';
  if (d.drop === 'older_than') {
    if (!/^\d+(\.\d+)?(ms|s|m|h|d)$/.test(d.olderThan) || /^0+(\.0+)?[a-z]+$/.test(d.olderThan))
      e.olderThan = 'Enter a duration above zero, such as 12h or 7d';
  } else if (!d.value.trim() && !valueFromTrigger(d.event, d.drop)) {
    e.value = d.drop === 'metadata' ? 'Enter key=value, such as prompt_version=v2' : 'Name what to clear';
  } else if (
    d.drop === 'metadata' &&
    d.value.trim() &&
    !/^[^=]+=.+$/.test(d.value.trim()) &&
    d.event !== 'metadata_changed'
  ) {
    e.value = 'Enter key=value, such as prompt_version=v2';
  }
  return e;
}

/** The rule a valid draft writes. */
export function specOf(d: RuleDraft): RuleSpec {
  const trigger: RuleSpec['trigger'] = { event: d.event };
  if (d.event === 'schedule') trigger.schedule = d.schedule.trim();
  if (d.event === 'metadata_changed') trigger.metadata_key = d.metadataKey.trim();
  const action: RuleSpec['action'] = { drop: d.drop };
  if (d.drop === 'older_than') action.older_than = d.olderThan;
  else if (d.value.trim()) action.value = d.value.trim();
  return {
    enabled: d.enabled,
    ...(d.description.trim() ? { description: d.description.trim() } : {}),
    trigger,
    action,
  };
}
