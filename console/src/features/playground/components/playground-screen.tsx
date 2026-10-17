'use client';

import { usePathname, useRouter, useSearchParams } from 'next/navigation';
import { useEffect, useMemo, useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, Segmented, Select, Textarea, type SelectOption } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { Forbidden, usePermission } from '@/components/ui/gate';
import { Kbd } from '@/components/ui/misc';
import { PageHeader, Panel, TileGrid } from '@/components/ui/layout';
import { ModelMark } from '@/components/ui/marks';
import { Banner, InlineError } from '@/components/ui/states';
import type { ProjectResource } from '@/lib/api/types';

import { useModelViews } from '@/features/models/hooks';
import { useCatalog, useModels } from '@/features/models/store';
import { storedBody } from '@/features/request-log/transcript';

import { playgroundApi } from '../api';
import {
  buildBody,
  composerErrors,
  emptyComposer,
  endpointOfPath,
  ENDPOINTS,
  isEndpoint,
  parseBody,
  parseObject,
  type Composer,
  type Endpoint,
} from '../body';
import { usePlaygroundDraft } from '../draft';
import { useSend } from '../use-send';
import { ComposerForm, Parameters } from './composer';
import { ResponsePanel } from './response-panel';

type Mode = 'form' | 'json';

function pretty(body: unknown): string {
  return JSON.stringify(body, null, 2);
}

/** The parameters set, in a few words, for the closed disclosure. */
function paramsSummary(c: Composer): string {
  const parts = [
    c.temperature ? `temperature ${c.temperature}` : null,
    c.topP ? `top P ${c.topP}` : null,
    c.maxTokens ? `max ${c.maxTokens} tokens` : null,
    c.stream ? 'streamed' : 'not streamed',
  ].filter(Boolean);
  return parts.join(' · ');
}

/**
 * The playground: compose a chat, Responses, Messages or embeddings request for a model, send it
 * through the gateway with a short-lived key the gateway keeps, and read the answer as it streams,
 * with the decision headers, the usage, cost and timing, and the request's record.
 */
export function PlaygroundScreen() {
  const allowed = usePermission('playground:send');
  if (!allowed)
    return (
      <>
        <PageHeader title="Playground" description="Try a model through the gateway" />
        <div className="flex min-h-[calc(100vh-48px-57px)] items-center justify-center bg-raised">
          <Forbidden what="the playground" />
        </div>
      </>
    );
  return <Playground />;
}

interface Editor {
  composer: Composer;
  mode: Mode;
  json: string;
  /** Whether the JSON is what gets sent (edited, or a body the form cannot say in full). */
  jsonEdited: boolean;
}

function editorOf(composer: Composer): Editor {
  return { composer, mode: 'form', json: pretty(buildBody(composer)), jsonEdited: false };
}

/** A body in the playground: in the form when the form says all of it, else as JSON. */
function editorFor(endpoint: Endpoint, body: Record<string, unknown>): Editor {
  const { composer, exact } = parseBody(endpoint, body);
  const rest = { ...body };
  delete rest.stream;
  delete rest.stream_options;
  return {
    composer,
    mode: exact ? 'form' : 'json',
    json: pretty(exact ? buildBody(composer) : rest),
    jsonEdited: !exact,
  };
}

const FROM_LOG = 'Opened from a logged request.';

function Playground() {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const [draft] = useState(() => usePlaygroundDraft.getState().draft);
  const [editor, setEditor] = useState<Editor>(() => {
    if (draft) return editorFor(draft.endpoint, draft.body);
    const endpoint = isEndpoint(params.get('endpoint'))
      ? (params.get('endpoint') as Endpoint)
      : 'chat_completions';
    return editorOf(emptyComposer(endpoint, params.get('model') ?? ''));
  });
  const { mode, json, jsonEdited } = editor;
  const [project, setProject] = useState<string>(() => params.get('project') ?? draft?.project ?? '');
  const [projects, setProjects] = useState<ProjectResource[] | null>(null);
  const [projectsError, setProjectsError] = useState<unknown>(null);
  const [source, setSource] = useState<{ id: string; note: string } | null>(() =>
    draft?.source ? { id: draft.source, note: FROM_LOG } : null,
  );
  const [showErrors, setShowErrors] = useState(false);
  const [paramsOpen, setParamsOpen] = useState(false);
  const { run, send, stop, clear } = useSend();
  const started = useRef(false);

  const fetchModels = useModels((s) => s.fetch);
  const modelsError = useModels((s) => s.error);
  const loadCatalog = useCatalog((s) => s.loadCatalog);
  const views = useModelViews();

  const fallbackModel = views.find((v) => v.enabled)?.name ?? views[0]?.name ?? '';
  const composer: Composer = editor.composer.model
    ? editor.composer
    : { ...editor.composer, model: fallbackModel };
  const projectName =
    project || (projects?.find((p) => p.name === 'default')?.name ?? projects?.[0]?.name ?? '');

  const setComposer = (next: Composer) =>
    setEditor((e) => ({ ...e, composer: next, json: e.jsonEdited ? e.json : pretty(buildBody(next)) }));
  const setMode = (m: Mode) =>
    setEditor((e) => ({
      ...e,
      mode: m,
      json: m === 'json' && !e.jsonEdited ? pretty(buildBody(composer)) : e.json,
    }));

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    usePlaygroundDraft.getState().set(null);
    void fetchModels();
    void loadCatalog();
    playgroundApi.projects().then(setProjects, setProjectsError);
    const requestId = params.get('request');
    if (draft || !requestId) return;
    playgroundApi.record(requestId).then(
      (record) => {
        const stored = storedBody(record?.content?.request);
        const endpoint = endpointOfPath(record?.endpoint);
        const body = stored?.body;
        if (
          !record ||
          !endpoint ||
          stored?.truncated ||
          typeof body !== 'object' ||
          body === null ||
          Array.isArray(body)
        ) {
          setSource({
            id: requestId,
            note: "That request's body wasn't stored in full, so the playground starts empty.",
          });
          return;
        }
        setEditor(editorFor(endpoint, body as Record<string, unknown>));
        if (record.project && !params.get('project')) setProject(record.project);
        setSource({ id: requestId, note: FROM_LOG });
      },
      () =>
        setSource({ id: requestId, note: "That request couldn't be read, so the playground starts empty." }),
    );
    // Runs once on open: later URL changes come from this screen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const next = new URLSearchParams(window.location.search);
    const put = (k: string, v: string) => (v ? next.set(k, v) : next.delete(k));
    put('model', composer.model);
    put('endpoint', composer.endpoint === 'chat_completions' ? '' : composer.endpoint);
    put('project', projectName);
    const query = next.toString();
    if (query !== window.location.search.replace(/^\?/, ''))
      router.replace(query ? `${pathname}?${query}` : pathname, { scroll: false });
  }, [composer.model, composer.endpoint, projectName, router, pathname]);

  const modelOptions: SelectOption[] = useMemo(
    () =>
      views.map((v) => {
        return {
          value: v.name,
          label: v.name,
          ...(v.providers.length ? { description: v.providers.join(', ') } : {}),
          icon: (
            <ModelMark
              model={v.name}
              ownedBy={v.model.spec.owned_by}
              family={v.model.spec.catalog?.family}
              upstream={v.upstream}
              size="md"
            />
          ),
          ...(v.enabled ? {} : { disabled: true }),
        };
      }),
    [views],
  );
  const projectOptions: SelectOption[] = (projects ?? []).map((p) => ({
    value: p.name,
    label: p.name,
    ...(p.spec.team ? { description: `Team ${p.spec.team}` } : {}),
  }));

  const errors = composerErrors(composer);
  const parsed = mode === 'json' ? parseObject(json) : null;
  const jsonError = parsed && 'error' in parsed ? parsed.error : null;
  const body = mode === 'json' ? (parsed && 'body' in parsed ? parsed.body : null) : buildBody(composer);
  const formBlocked = mode === 'form' && Object.keys(errors).length > 0;
  const live = run?.status === 'sending' || run?.status === 'streaming';

  function setEndpoint(endpoint: Endpoint) {
    setComposer({
      ...composer,
      endpoint,
      stream: endpoint === 'embeddings' ? false : composer.stream || composer.endpoint === 'embeddings',
      maxTokens: endpoint === 'messages' && !composer.maxTokens ? '1024' : composer.maxTokens,
    });
  }

  const changeForm = setComposer;

  function submit() {
    if (live) return;
    if (formBlocked || !body || !projectName) {
      setShowErrors(true);
      return;
    }
    setShowErrors(false);
    send({
      project: projectName,
      endpoint: composer.endpoint,
      body: {
        ...body,
        ...(mode === 'json' && typeof body.model === 'string' ? {} : { model: composer.model }),
      },
      stream: composer.endpoint !== 'embeddings' && composer.stream,
    });
  }

  function continueConversation() {
    if (!run?.answer.text || composer.endpoint === 'embeddings') return;
    const answered = run.answer.text;
    const turns = composer.turns.filter((t) => t.text.trim());
    setEditor(
      editorOf({
        ...composer,
        turns: [...turns, { role: 'assistant', text: answered }, { role: 'user', text: '' }],
      }),
    );
  }

  const visibleErrors = showErrors ? errors : {};
  return (
    <>
      <PageHeader
        title="Playground"
        description="Send a request through the gateway, as your apps do, and see how it was handled"
        context={projectName ? `Project ${projectName}` : undefined}
        actions={
          <Button
            variant="ghost"
            icon="refresh"
            onClick={() => {
              clear();
              setSource(null);
              setEditor(editorOf(emptyComposer(composer.endpoint, composer.model)));
            }}
          >
            Start over
          </Button>
        }
      />
      {source ? (
        <Banner tone="info" title={source.note}>
          A send here is a new request, billed and logged like any other; it is not marked as a replay.
        </Banner>
      ) : null}
      <TileGrid label="Playground" className="grid-cols-1 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <Panel
          title="Request"
          actions={
            <Segmented<Mode>
              label="Edit the request as"
              value={mode}
              onChange={setMode}
              options={[
                { value: 'form', label: 'Form' },
                { value: 'json', label: 'JSON' },
              ]}
            />
          }
          bodyClassName="flex flex-col gap-4"
        >
          <div className="grid grid-cols-1 gap-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
            <Field
              label="Project"
              hint="The short-lived key is made in this project."
              error={projectsError ? 'Projects could not be listed; type a project name.' : undefined}
            >
              {projects && !projectsError ? (
                <Select
                  searchable
                  options={projectOptions}
                  value={projectName}
                  onChange={setProject}
                  placeholder="Choose a project"
                />
              ) : (
                <Input
                  mono
                  value={project}
                  onChange={(e) => setProject(e.target.value)}
                  placeholder="default"
                  aria-label="Project"
                />
              )}
            </Field>
            <Field label="Model" error={visibleErrors.model}>
              {modelsError ? (
                <Input
                  mono
                  value={composer.model}
                  onChange={(e) => changeForm({ ...composer, model: e.target.value })}
                />
              ) : (
                <Select
                  searchable
                  searchLabel="Search models"
                  options={modelOptions}
                  value={composer.model}
                  onChange={(model) => changeForm({ ...composer, model })}
                  placeholder="Choose a model"
                />
              )}
            </Field>
          </div>
          <div className="flex flex-col gap-1.5">
            <span className="text-ui font-medium text-fg">Endpoint</span>
            <Segmented<Endpoint>
              label="Endpoint"
              className="self-start max-sm:w-full max-sm:overflow-x-auto"
              value={composer.endpoint}
              onChange={setEndpoint}
              options={ENDPOINTS.map((e) => ({ value: e.value, label: e.label }))}
            />
          </div>
          {mode === 'form' ? (
            <>
              {jsonEdited ? (
                <p
                  role="status"
                  className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-control border border-[color-mix(in_oklab,var(--ant-warn)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-warn)_8%,transparent)] px-3 py-2 text-ui text-fg"
                >
                  <Icon name="alert" size={16} className="shrink-0 text-warn" />
                  <span className="min-w-0 flex-1">
                    The JSON holds more than this form shows, so the JSON is what gets sent.
                  </span>
                  <button
                    type="button"
                    className="font-medium text-accent hover:underline"
                    onClick={() => setEditor(editorOf(composer))}
                  >
                    Send the form instead
                  </button>
                </p>
              ) : null}
              <ComposerForm value={composer} onChange={changeForm} errors={visibleErrors} onSubmit={submit} />
              {composer.endpoint !== 'embeddings' ? (
                <div className="-mx-5 border-y border-line max-sm:-mx-gutter-sm">
                  <button
                    type="button"
                    aria-expanded={paramsOpen}
                    aria-controls="pg-parameters"
                    onClick={() => setParamsOpen((o) => !o)}
                    className="flex w-full items-center justify-between gap-2 px-5 py-2.5 text-left text-ui font-medium text-fg hover:bg-hover max-sm:px-gutter-sm"
                  >
                    <span className="flex items-center gap-2">
                      <Icon name="sliders" size={14} className="text-muted" />
                      Parameters
                    </span>
                    <span className="flex items-center gap-2 text-caption font-normal text-muted">
                      {paramsSummary(composer)}
                      <Icon name={paramsOpen ? 'chevronUp' : 'chevronDown'} size={14} />
                    </span>
                  </button>
                  {paramsOpen ? (
                    <div id="pg-parameters" className="border-t border-line px-5 py-3 max-sm:px-gutter-sm">
                      <Parameters value={composer} onChange={changeForm} errors={visibleErrors} />
                    </div>
                  ) : null}
                </div>
              ) : null}
            </>
          ) : (
            <Field
              label="Request body"
              hint="Sent as written; the model chosen above fills in a body without one."
              error={jsonError ?? undefined}
            >
              <Textarea
                mono
                spellCheck={false}
                rows={18}
                value={json}
                onChange={(e) => {
                  const text = e.target.value;
                  setEditor((ed) => ({ ...ed, json: text, jsonEdited: true }));
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    submit();
                  }
                }}
              />
            </Field>
          )}
          <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-3">
            <p className="text-caption text-muted">
              Billed and logged like any other request. <Kbd>⌘</Kbd> <Kbd>Enter</Kbd> sends.
            </p>
            {live ? (
              <Button icon="pause" onClick={stop}>
                Stop
              </Button>
            ) : (
              <Button
                variant="primary"
                icon="send"
                onClick={submit}
                disabled={!projectName || (mode === 'json' && !body)}
                disabledReason={!projectName ? 'Choose a project first.' : (jsonError ?? undefined)}
              >
                Send
              </Button>
            )}
          </div>
          {showErrors && formBlocked ? (
            <InlineError error={new Error('Some fields need attention.')} />
          ) : null}
        </Panel>
        <Panel title="Response" bodyClassName="flex flex-col gap-4">
          <ResponsePanel
            run={run}
            onContinue={composer.endpoint === 'embeddings' ? null : continueConversation}
          />
        </Panel>
      </TileGrid>
    </>
  );
}
