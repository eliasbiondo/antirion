'use client';

import type { ReactNode } from 'react';

import { cn } from '@/lib/cn';
import { type MarkFace, type ModelMakerHints, modelFace, providerFace } from '@/lib/marks';
import { useModelHints } from '@/stores/model-makers';

import { BRAND_MARKS } from './brand-marks';
import type { SelectOption } from './select';

/**
 * Mark sizes: `xs` 12px in chips and badges, `sm` 14px beside a name in rows and tables, `md` 16px
 * in menus, options and drawer titles, `lg` 18px in a `MarkTile`.
 */
export type MarkSize = 'xs' | 'sm' | 'md' | 'lg';

const BOX: Record<MarkSize, string> = {
  xs: 'h-3 w-3',
  sm: 'h-3.5 w-3.5',
  md: 'h-4 w-4',
  lg: 'h-[18px] w-[18px]',
};
const PX: Record<MarkSize, number> = { xs: 12, sm: 14, md: 16, lg: 18 };
const LETTER: Record<MarkSize, string> = {
  xs: 'text-[8px]',
  sm: 'text-[9px]',
  md: 'text-[10px]',
  lg: 'text-[11px]',
};

/**
 * One mark: a brand's glyph or a neutral monogram, in the current text colour (both themes), square
 * and pixel-sized so it lines up with the text beside it. Decorative: the name is always next to it.
 */
export function Mark({
  face,
  size = 'sm',
  className,
}: {
  face: MarkFace;
  size?: MarkSize;
  className?: string;
}) {
  if (face.brand === null) {
    return (
      <span
        aria-hidden="true"
        data-mark="monogram"
        className={cn(
          'inline-flex shrink-0 items-center justify-center rounded-[3px] border border-current leading-none font-semibold',
          BOX[size],
          LETTER[size],
          className,
        )}
      >
        {face.letter}
      </span>
    );
  }
  return (
    <svg
      aria-hidden="true"
      data-mark={face.brand}
      viewBox="0 0 24 24"
      width={PX[size]}
      height={PX[size]}
      fill="currentColor"
      fillRule="evenodd"
      className={cn('block shrink-0', BOX[size], className)}
    >
      {BRAND_MARKS[face.brand].paths}
    </svg>
  );
}

export interface ProviderMarkProps {
  /** The provider's type (`openai`, `bedrock`…): its brand, or a monogram for generic and unknown types. */
  type: string | null | undefined;
  size?: MarkSize;
  className?: string;
}

/** A provider's mark: its type's brand. Every screen draws providers with this. */
export function ProviderMark({ type, size, className }: ProviderMarkProps) {
  return <Mark face={providerFace(type)} size={size ?? 'sm'} className={className} />;
}

export interface ModelMarkProps extends Omit<ModelMakerHints, 'name' | 'upstream'> {
  /** The model's name (the gateway's, as a record or a list names it). */
  model: string | null | undefined;
  /** The upstream model id a record says served it, or a model's own upstream ids. */
  upstream?: string | null | readonly (string | null | undefined)[];
  size?: MarkSize;
  className?: string;
}

/**
 * A model's mark: its maker's brand, never the provider that served it (`claude-*` is Anthropic's
 * on any provider), from the model's metadata, its deployments' upstream ids, then its name.
 */
export function ModelMark({ model, upstream, ownedBy, family, size, className }: ModelMarkProps) {
  const face = useModelFace(model, upstream, { ownedBy, family });
  return <Mark face={face} size={size ?? 'sm'} className={className} />;
}

/** A model's face (its maker's brand and name), for a mark and the words beside it. */
export function useModelFace(
  model: string | null | undefined,
  upstream?: string | null | readonly (string | null | undefined)[],
  explicit?: Pick<ModelMakerHints, 'ownedBy' | 'family'>,
): MarkFace {
  const hintsOf = useModelHints();
  return modelFace(mergeHints(hintsOf(model), upstream, explicit));
}

/** A function from a model to its face, for lists that draw many (charts, options). */
export function useModelFaces(): (
  model: string | null | undefined,
  upstream?: string | null | readonly (string | null | undefined)[],
) => MarkFace {
  const hintsOf = useModelHints();
  return (model, upstream) => modelFace(mergeHints(hintsOf(model), upstream));
}

function mergeHints(
  known: ModelMakerHints,
  upstream: string | null | undefined | readonly (string | null | undefined)[],
  explicit?: Pick<ModelMakerHints, 'ownedBy' | 'family'>,
): ModelMakerHints {
  const extra = typeof upstream === 'string' || upstream == null ? [upstream] : upstream;
  return {
    ...known,
    ownedBy: explicit?.ownedBy ?? known.ownedBy,
    family: explicit?.family ?? known.family,
    upstream: [...(known.upstream ?? []), ...extra],
  };
}

/**
 * The square tile a list row leads with (the Models list, as the prototype draws a model): the mark
 * at `lg` on the sunken background; a monogram drops its own border inside it.
 */
export function MarkTile({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'relative flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-line bg-sunken text-fg',
        '[&>[data-mark=monogram]]:border-0 [&>[data-mark=monogram]]:text-ui',
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A provider's mark and name together: "[mark] OpenAI" or "[mark] azure-east". */
export function ProviderName({
  type,
  name,
  className,
}: {
  type: string | null | undefined;
  /** The provider's own name; its type's label when absent. */
  name?: string | undefined;
  className?: string;
}) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1.5', className)}>
      <ProviderMark type={type} className="text-muted" />
      <span className="truncate">{name ?? providerFace(type).label}</span>
    </span>
  );
}

/** A model's mark and name together, the mark muted: "[mark] claude-sonnet". */
export function ModelName({
  model,
  upstream,
  className,
  mono = true,
}: {
  model: string;
  upstream?: string | null;
  className?: string;
  mono?: boolean;
}) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1.5', className)}>
      <ModelMark model={model} upstream={upstream} className="text-muted" />
      <span className={cn('truncate', mono && 'mono')}>{model}</span>
    </span>
  );
}

/** A model as a select or combobox option, with its maker's mark: every list of models uses this. */
export function modelOption(
  model: string | { name: string; spec?: { owned_by?: string; catalog?: { family?: string } } },
  label?: string,
): SelectOption {
  const name = typeof model === 'string' ? model : model.name;
  const spec = typeof model === 'string' ? undefined : model.spec;
  return {
    value: name,
    label: label ?? name,
    icon: <ModelMark model={name} ownedBy={spec?.owned_by} family={spec?.catalog?.family} size="md" />,
  };
}

/** A mark inside an SVG chart (a label's lead), `size` px square with its top left at `x`, `y`. */
export function SvgMark({ face, x, y, size = 12 }: { face: MarkFace; x: number; y: number; size?: number }) {
  if (face.brand === null) {
    return (
      <g aria-hidden="true" data-mark="monogram">
        <rect
          x={x + 0.5}
          y={y + 0.5}
          width={size - 1}
          height={size - 1}
          rx={2.5}
          className="fill-none stroke-current"
        />
        <text
          x={x + size / 2}
          y={y + size / 2 + size * 0.27}
          textAnchor="middle"
          fontSize={size * 0.68}
          fontWeight={600}
          className="fill-current"
        >
          {face.letter}
        </text>
      </g>
    );
  }
  return (
    <svg
      aria-hidden="true"
      data-mark={face.brand}
      x={x}
      y={y}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      fillRule="evenodd"
    >
      {BRAND_MARKS[face.brand].paths}
    </svg>
  );
}
