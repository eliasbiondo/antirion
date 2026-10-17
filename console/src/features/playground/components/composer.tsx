'use client';

import { Button, IconButton } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { Field, NumberInput, Segmented, Textarea } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { cn } from '@/lib/cn';

import type { Composer, Role, Turn } from '../body';

type Errors = Partial<Record<keyof Composer, string>>;

const ROLE_LABEL: Record<Role, string> = { user: 'User', assistant: 'Assistant' };

/** One message of the conversation: its role, its text, and remove. */
function TurnField({
  turn,
  index,
  onChange,
  onRemove,
  onSubmit,
}: {
  turn: Turn;
  index: number;
  onChange: (turn: Turn) => void;
  onRemove: (() => void) | null;
  onSubmit: () => void;
}) {
  const id = `turn-${index}`;
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <Segmented
          label={`Role of message ${index + 1}`}
          value={turn.role}
          onChange={(role) => onChange({ ...turn, role })}
          options={(['user', 'assistant'] as const).map((r) => ({ value: r, label: ROLE_LABEL[r] }))}
        />
        {onRemove ? (
          <IconButton icon="x" label={`Remove message ${index + 1}`} onClick={onRemove} size={14} />
        ) : null}
      </div>
      <Textarea
        id={id}
        aria-label={`${ROLE_LABEL[turn.role]} message ${index + 1}`}
        rows={turn.role === 'user' ? 4 : 3}
        value={turn.text}
        placeholder={turn.role === 'user' ? 'Write a message' : 'What the assistant said'}
        onChange={(e) => onChange({ ...turn, text: e.target.value })}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            onSubmit();
          }
        }}
      />
    </div>
  );
}

/** The form for a chat, Responses, Messages or embeddings request. */
export function ComposerForm({
  value,
  onChange,
  errors,
  onSubmit,
}: {
  value: Composer;
  onChange: (next: Composer) => void;
  errors: Errors;
  onSubmit: () => void;
}) {
  const set = <K extends keyof Composer>(key: K, v: Composer[K]) => onChange({ ...value, [key]: v });
  if (value.endpoint === 'embeddings') {
    return (
      <Field label="Input" hint="One text per line; each gets its own vector." error={errors.input}>
        <Textarea
          rows={6}
          value={value.input}
          placeholder="Text to embed"
          onChange={(e) => set('input', e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              onSubmit();
            }
          }}
        />
      </Field>
    );
  }
  const turns = value.turns;
  return (
    <div className="flex flex-col gap-4">
      <Field label="System instructions" optional>
        <Textarea
          rows={2}
          value={value.system}
          placeholder="How the model should answer"
          onChange={(e) => set('system', e.target.value)}
        />
      </Field>
      <fieldset className="flex flex-col gap-3">
        <legend className="mb-1.5 text-ui font-medium text-fg">Conversation</legend>
        {turns.map((turn, i) => (
          <TurnField
            key={i}
            turn={turn}
            index={i}
            onSubmit={onSubmit}
            onChange={(t) =>
              set(
                'turns',
                turns.map((x, j) => (j === i ? t : x)),
              )
            }
            onRemove={
              turns.length > 1
                ? () =>
                    set(
                      'turns',
                      turns.filter((_, j) => j !== i),
                    )
                : null
            }
          />
        ))}
        {errors.turns ? (
          <p role="alert" className="flex items-start gap-1.5 text-caption text-error">
            <Icon name="alertCircle" size={14} className="mt-px shrink-0" />
            {errors.turns}
          </p>
        ) : null}
        <div>
          <Button
            size="sm"
            variant="ghost"
            icon="plus"
            onClick={() =>
              set('turns', [
                ...turns,
                { role: turns.at(-1)?.role === 'user' ? 'assistant' : 'user', text: '' },
              ])
            }
          >
            Add message
          </Button>
        </div>
      </fieldset>
    </div>
  );
}

/** Temperature, top P, the answer's cap, stop sequences and streaming. */
export function Parameters({
  value,
  onChange,
  errors,
}: {
  value: Composer;
  onChange: (next: Composer) => void;
  errors: Errors;
}) {
  const set = <K extends keyof Composer>(key: K, v: Composer[K]) => onChange({ ...value, [key]: v });
  if (value.endpoint === 'embeddings') return null;
  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Field label="Temperature" optional error={errors.temperature} hint="0 to 2">
          <NumberInput
            value={value.temperature}
            onChange={(v) => set('temperature', v)}
            min={0}
            max={2}
            step={0.1}
          />
        </Field>
        <Field label="Top P" optional error={errors.topP} hint="0 to 1">
          <NumberInput value={value.topP} onChange={(v) => set('topP', v)} min={0} max={1} step={0.05} />
        </Field>
        <Field
          label="Max output tokens"
          optional={value.endpoint !== 'messages'}
          required={value.endpoint === 'messages'}
          error={errors.maxTokens}
        >
          <NumberInput value={value.maxTokens} onChange={(v) => set('maxTokens', v)} min={1} step={64} />
        </Field>
      </div>
      {value.endpoint !== 'responses' ? (
        <Field label="Stop sequences" optional hint="One per line, up to four." error={errors.stop}>
          <Textarea
            rows={2}
            mono
            value={value.stop}
            onChange={(e) => set('stop', e.target.value)}
            className="min-h-14"
          />
        </Field>
      ) : null}
      <div
        className={cn('flex items-center justify-between gap-3 rounded-control border border-line px-3 py-2')}
      >
        <div className="min-w-0">
          <p className="text-ui font-medium text-fg">Stream the answer</p>
          <p className="text-caption text-muted">Words show as the model writes them.</p>
        </div>
        <Switch
          checked={value.stream}
          onChange={(v) => set('stream', v)}
          label="Stream the answer"
          hideLabel
        />
      </div>
    </div>
  );
}
