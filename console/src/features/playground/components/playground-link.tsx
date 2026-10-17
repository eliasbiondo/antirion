'use client';

import Link from 'next/link';
import type { ReactNode } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { buttonClass, type ButtonSize, type ButtonVariant } from '@/components/ui/button';
import { usePermission } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { cn } from '@/lib/cn';

import type { Endpoint } from '../body';
import { usePlaygroundDraft } from '../draft';

export interface PlaygroundLinkProps {
  /** The model to start with. */
  model?: string | undefined;
  endpoint?: Endpoint | undefined;
  project?: string | null | undefined;
  /** A body to open with (a logged request, perhaps edited); it goes through memory, never the URL. */
  body?: Record<string, unknown> | undefined;
  /** The record the body came from. */
  source?: string | undefined;
  variant?: ButtonVariant;
  size?: ButtonSize;
  children?: ReactNode;
  /** Called after the link is followed (a dialog closes itself). */
  onOpen?: () => void;
  className?: string;
}

/**
 * "Try in playground": opens the playground with a model, endpoint, project or body chosen. Shown
 * only to principals who may send through the playground; any screen can drop it in.
 */
export function PlaygroundLink({
  model,
  endpoint,
  project,
  body,
  source,
  variant = 'secondary',
  size = 'md',
  children = 'Try in playground',
  onOpen,
  className,
}: PlaygroundLinkProps) {
  const allowed = usePermission('playground:send');
  const params: Record<string, string> = {};
  if (model) params.model = model;
  if (endpoint) params.endpoint = endpoint;
  if (project) params.project = project;
  if (source) params.request = source;
  const href = useScopedHref('/playground/', params);
  const setDraft = usePlaygroundDraft((s) => s.set);
  if (!allowed) return null;
  return (
    <Link
      href={href}
      className={cn(buttonClass(variant, size), className)}
      onClick={() => {
        setDraft(
          body && endpoint ? { endpoint, body, project: project ?? null, source: source ?? null } : null,
        );
        onOpen?.();
      }}
    >
      <Icon name="play" size={size === 'sm' ? 14 : 16} />
      {children}
    </Link>
  );
}
