'use client';

import Link from 'next/link';

import { useScopedHref } from '@/components/layout/url-sync';
import { buttonClass } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { CopyButton } from '@/components/ui/misc';
import { cn } from '@/lib/cn';
import { useShell } from '@/stores/shell';

import { chatUrl, curlSnippet, type SetupState } from '../setup';

interface Step {
  id: string;
  title: string;
  description: string;
  done: boolean | null;
  action?: { label: string; href: string; query?: Record<string, string> };
}

function StepLink({
  href,
  query,
  label,
  primary,
}: {
  href: string;
  query?: Record<string, string>;
  label: string;
  primary: boolean;
}) {
  const scoped = useScopedHref(href, query);
  return (
    <Link href={scoped} className={buttonClass(primary ? 'primary' : 'secondary', 'sm')}>
      {label}
    </Link>
  );
}

/**
 * The first-run path on the Overview, until the gateway has served a request: a checklist that
 * ticks from real reads (provider, model, key, first request) and a copyable request to send.
 */
export function GetStarted({ setup }: { setup: SetupState }) {
  const dataPlaneUrl = useShell((s) => s.status?.data_plane_url ?? null);
  const steps: Step[] = [
    {
      id: 'provider',
      title: 'Add a provider',
      description: 'Connect the service the gateway calls, with its credential.',
      done: setup.hasProvider,
      action: { label: 'Add provider', href: '/providers/' },
    },
    {
      id: 'model',
      title: 'Add a model',
      description: 'Name a provider model the way your apps will ask for it.',
      done: setup.hasModel,
      action: { label: 'Add model', href: '/models/' },
    },
    {
      id: 'key',
      title: 'Create an API key',
      description: 'Apps send it with every request; you see it only once.',
      done: setup.hasKey,
      action: { label: 'Create key', href: '/keys/' },
    },
    {
      id: 'request',
      title: 'Send your first request',
      description: 'Run the request on the right. It appears below within seconds.',
      done: setup.hasRequest,
    },
  ];
  const doneCount = steps.filter((s) => s.done === true).length;
  const next = steps.find((s) => s.done !== true);
  const snippet = curlSnippet(dataPlaneUrl, setup.modelName);
  const baseUrl = chatUrl(dataPlaneUrl).replace(/\/chat\/completions$/, '');

  return (
    <section
      aria-labelledby="get-started-title"
      className="grid gap-px border-b border-line bg-line lg:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)]"
    >
      <div className="bg-raised px-gutter py-5 max-sm:px-gutter-sm">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 id="get-started-title" className="text-base font-semibold text-fg">
              Get started
            </h2>
            <p className="mt-0.5 text-ui text-muted">Four steps to your first request through the gateway.</p>
          </div>
          <span className="shrink-0 text-caption text-muted tabular-nums">
            {doneCount} of {steps.length} done
          </span>
        </div>
        <ol className="mt-4 flex flex-col">
          {steps.map((step, i) => {
            const isNext = step === next;
            return (
              <li
                key={step.id}
                className="flex items-center gap-3 border-t border-line py-3 first:border-t-0"
                aria-current={isNext ? 'step' : undefined}
              >
                <span
                  className={cn(
                    'flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-caption font-semibold tabular-nums',
                    step.done === true
                      ? 'text-success'
                      : isNext
                        ? 'border border-accent text-accent'
                        : 'border border-line-strong text-muted',
                  )}
                >
                  {step.done === true ? <Icon name="checkCircleFilled" size={24} label="Done" /> : i + 1}
                </span>
                <div className="min-w-0 flex-1">
                  <p className={cn('text-ui font-medium', step.done === true ? 'text-muted' : 'text-fg')}>
                    {step.title}
                  </p>
                  <p className="text-caption text-muted">{step.description}</p>
                </div>
                {step.action && step.done !== true ? (
                  <StepLink
                    href={step.action.href}
                    label={step.action.label}
                    primary={isNext}
                    {...(step.action.query ? { query: step.action.query } : {})}
                  />
                ) : null}
              </li>
            );
          })}
        </ol>
      </div>
      <div className="flex min-w-0 flex-col bg-raised px-gutter py-5 max-sm:px-gutter-sm">
        <div className="flex items-center justify-between gap-3">
          <h3 className="text-ui font-semibold text-fg">Send a request</h3>
          <span className="flex items-center gap-2 text-caption text-muted" role="status">
            <span className="relative flex h-2 w-2" aria-hidden="true">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-accent opacity-60" />
              <span className="relative inline-flex h-2 w-2 rounded-full bg-accent" />
            </span>
            Waiting for the first request
          </span>
        </div>
        <p className="mt-1 text-ui text-muted">
          Set <code className="mono text-fg">ANTIRION_API_KEY</code> to a key you created, then run:
        </p>
        <div className="relative mt-3 mb-4 min-w-0">
          <pre className="mono overflow-x-auto rounded-control border border-line bg-sunken py-3 pr-12 pl-3.5 text-caption leading-5 text-fg">
            {snippet}
          </pre>
          <div className="absolute top-1.5 right-1.5">
            <CopyButton value={snippet} label="Copy the request" />
          </div>
        </div>
        {dataPlaneUrl ? null : (
          <p className="mt-2 text-caption text-muted">
            Replace the address with your gateway&apos;s public URL; an admin can set it in the gateway
            configuration.
          </p>
        )}
        <div className="mt-auto border-t border-line pt-4">
          <p className="text-ui font-medium text-fg">Using an SDK?</p>
          <p className="mt-0.5 text-caption text-muted">
            Clients for the OpenAI Chat Completions API work unchanged: set their base URL to the gateway and
            their API key to one you created.
          </p>
          <div className="mt-2 flex min-w-0 items-center gap-1 rounded-control border border-line bg-sunken py-1 pr-1 pl-3">
            <span className="text-caption text-dim">Base URL</span>
            <code className="mono ml-2 min-w-0 flex-1 truncate text-caption text-fg">{baseUrl}</code>
            <CopyButton value={baseUrl} label="Copy the base URL" />
          </div>
        </div>
      </div>
    </section>
  );
}
