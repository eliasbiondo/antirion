'use client';

import Link from 'next/link';
import { Fragment, useEffect, useState } from 'react';

import { useSettingsHref } from '@/components/layout/url-sync';
import { useSwitchChanges } from '@/components/ui/confirm';
import { Checkbox, Field, Input, NumberInput, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { LoadError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { describeError, isChangeControl } from '@/lib/api/client';
import { SETTINGS_DEFAULTS } from '@/lib/settings-defaults';
import type { FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { DURATION_RE } from '../form-errors';
import { useGatewaySettings } from '../store';
import { Group, SettingsForm, SIZE_RE, useSettingsForm } from './gateway-forms';
import { Card, Facts } from './parts';

export interface EndpointRow {
  id: string;
  label: string;
  paths: string;
  /** Answers only while this other endpoint is on. */
  needs?: string;
}

/** The endpoints apps call, by the API they speak, with the paths each one serves. */
export const ENDPOINT_GROUPS: { title: string; rows: EndpointRow[] }[] = [
  {
    title: 'OpenAI',
    rows: [
      { id: 'chat_completions', label: 'Chat completions', paths: 'POST /v1/chat/completions' },
      { id: 'responses', label: 'Responses', paths: '/v1/responses' },
      {
        id: 'responses_websocket',
        label: 'Responses over WebSocket',
        paths: 'GET /v1/responses (upgrade)',
        needs: 'responses',
      },
      { id: 'completions', label: 'Completions (legacy)', paths: 'POST /v1/completions' },
      { id: 'embeddings', label: 'Embeddings', paths: 'POST /v1/embeddings' },
      { id: 'images', label: 'Images', paths: '/v1/images/*' },
      { id: 'audio', label: 'Audio', paths: '/v1/audio/*' },
      { id: 'moderations', label: 'Moderations', paths: 'POST /v1/moderations' },
      { id: 'files', label: 'Files', paths: '/v1/files' },
      { id: 'batches', label: 'Batches', paths: '/v1/batches' },
      { id: 'realtime', label: 'Realtime', paths: '/v1/realtime' },
      { id: 'models', label: 'Model list', paths: 'GET /v1/models' },
    ],
  },
  {
    title: 'Anthropic',
    rows: [
      { id: 'messages', label: 'Messages', paths: 'POST /v1/messages' },
      { id: 'count_tokens', label: 'Token counting', paths: 'POST /v1/messages/count_tokens' },
    ],
  },
  {
    title: 'Gemini',
    rows: [{ id: 'gemini', label: 'Gemini API', paths: '/v1beta/models/{model}:{action}' }],
  },
  {
    title: 'Bedrock',
    rows: [{ id: 'bedrock', label: 'Bedrock runtime', paths: '/model/{model}/converse, /invoke' }],
  },
  {
    title: 'Other',
    rows: [
      { id: 'rerank', label: 'Rerank', paths: 'POST /v1/rerank, /v2/rerank' },
      { id: 'systemone', label: 'Typed decisions', paths: 'POST /v1/systemone' },
    ],
  },
];

const ROWS = ENDPOINT_GROUPS.flatMap((g) => g.rows);

type Endpoints = Record<string, { enabled?: boolean; default_provider?: string | null } | undefined>;
type ApiSpec = {
  endpoints?: Endpoints;
  provider_mounts?: boolean;
  azure_paths?: boolean;
  response_model?: string;
  responses?: { state?: string; ttl?: string; max_stored_bytes?: string; max_chain_depth?: number };
  reasoning_replay?: { enabled?: boolean; ttl?: string; max_entry_bytes?: string };
  media_fetch?: { enabled?: boolean; max_total_bytes?: string; max_objects?: number };
  images?: { max_n?: number };
  passthrough?: Record<string, unknown>;
};

/** Whether an endpoint is on: every endpoint is on unless the configuration turns it off. */
export const endpointOn = (endpoints: Endpoints | undefined, id: string): boolean =>
  endpoints?.[id]?.enabled !== false;

function EndpointsCard({ spec }: { spec: ApiSpec }) {
  const saveApi = useGatewaySettings((s) => s.saveApi);
  const canEdit = useCan('patchApiSettings');
  const { change, dialog } = useSwitchChanges();
  const [providers, setProviders] = useState<string[] | null>(null);
  const canReadProviders = useCan('listProviders');
  useEffect(() => {
    if (!canReadProviders) return;
    settingsApi.providers().then(
      (list) => setProviders(list.map((p) => p.name)),
      () => setProviders(null),
    );
  }, [canReadProviders]);
  const endpoints = spec.endpoints;
  const offCount = ROWS.filter((r) => !endpointOn(endpoints, r.id)).length;
  const setEnabled = (id: string, enabled: boolean) => saveApi({ endpoints: { [id]: { enabled } } });
  const filesProvider = endpoints?.files?.default_provider ?? null;
  return (
    <Card
      title="Endpoints"
      description={
        offCount
          ? `${offCount} of ${ROWS.length} turned off. Apps calling a turned-off endpoint get 404.`
          : 'Every endpoint is on. Apps calling a turned-off endpoint get 404.'
      }
    >
      <Table label="Endpoints">
        <thead>
          <tr>
            <Th>Endpoint</Th>
            <Th>Paths</Th>
            <Th>
              <span className="sr-only">On</span>
            </Th>
          </tr>
        </thead>
        <tbody>
          {ENDPOINT_GROUPS.map((group) => (
            <Fragment key={group.title}>
              <tr>
                <th
                  scope="colgroup"
                  colSpan={3}
                  className="border-b border-line px-[var(--spacing-cell)] pt-4 pb-1.5 text-left text-caption font-semibold text-muted"
                >
                  {group.title}
                </th>
              </tr>
              {group.rows.map((row) => {
                const on = endpointOn(endpoints, row.id);
                const blocked = row.needs ? !endpointOn(endpoints, row.needs) : false;
                return (
                  <Tr key={row.id}>
                    <Td>{row.label}</Td>
                    <Td mono muted>
                      {row.paths}
                    </Td>
                    <Td className="w-12 text-right">
                      <Switch
                        label={`${row.label} endpoint`}
                        hideLabel
                        checked={on && !blocked}
                        disabled={!canEdit || blocked}
                        disabledReason={
                          blocked ? 'Answers only while Responses is on' : "Your role can't change endpoints"
                        }
                        onChange={(next) =>
                          change({
                            name: row.label,
                            apply: () => setEnabled(row.id, next),
                            undo: () => setEnabled(row.id, !next),
                            done: `${row.label} turned ${next ? 'on' : 'off'}`,
                            undone: `${row.label} turned ${next ? 'off' : 'on'}`,
                            ask: next
                              ? undefined
                              : {
                                  title: `Turn off ${row.label}?`,
                                  effect: `Apps calling ${row.paths} get 404 until it is turned back on.`,
                                  confirmLabel: 'Turn off',
                                },
                          })
                        }
                      />
                    </Td>
                  </Tr>
                );
              })}
            </Fragment>
          ))}
        </tbody>
      </Table>
      {providers ? (
        <Field
          label="Files are stored with"
          className="mt-4 max-w-80"
          hint="When an upload doesn't name a provider."
        >
          <Select
            value={filesProvider ?? ''}
            disabled={!canEdit}
            onChange={(name) =>
              void saveApi({ endpoints: { files: { default_provider: name || null } } }).then(
                () => notify('success', 'Files setting saved'),
                (e: unknown) => {
                  // Change control says so itself; anything else is a failure to show.
                  if (!isChangeControl(e))
                    notify('error', "Couldn't save the Files setting", describeError(e));
                },
              )
            }
            options={[
              { value: '', label: 'The upload must name one' },
              ...providers.map((p) => ({ value: p, label: p })),
            ]}
          />
        </Field>
      ) : null}
      {dialog}
    </Card>
  );
}

const AD = SETTINGS_DEFAULTS.api;

interface BehaviourValues {
  providerMounts: boolean;
  azurePaths: boolean;
  responseModel: string;
  state: string;
  ttl: string;
  maxStored: string;
  maxDepth: string;
  replay: boolean;
  replayTtl: string;
  replayMax: string;
  mediaFetch: boolean;
  mediaTotal: string;
  mediaObjects: string;
  imagesMax: string;
}

export function behaviourValues(spec: ApiSpec): BehaviourValues {
  return {
    providerMounts: spec.provider_mounts ?? AD.provider_mounts,
    azurePaths: spec.azure_paths ?? AD.azure_paths,
    responseModel: spec.response_model ?? AD.response_model,
    state: spec.responses?.state ?? AD.responses.state,
    ttl: spec.responses?.ttl ?? AD.responses.ttl,
    maxStored: spec.responses?.max_stored_bytes ?? AD.responses.max_stored_bytes,
    maxDepth: String(spec.responses?.max_chain_depth ?? AD.responses.max_chain_depth),
    replay: spec.reasoning_replay?.enabled ?? AD.reasoning_replay.enabled,
    replayTtl: spec.reasoning_replay?.ttl ?? AD.reasoning_replay.ttl,
    replayMax: spec.reasoning_replay?.max_entry_bytes ?? AD.reasoning_replay.max_entry_bytes,
    mediaFetch: spec.media_fetch?.enabled ?? AD.media_fetch.enabled,
    mediaTotal: spec.media_fetch?.max_total_bytes ?? AD.media_fetch.max_total_bytes,
    mediaObjects: String(spec.media_fetch?.max_objects ?? AD.media_fetch.max_objects),
    imagesMax: String(spec.images?.max_n ?? AD.images.max_n),
  };
}

const behaviourSpec = (v: BehaviourValues) => ({
  provider_mounts: v.providerMounts,
  azure_paths: v.azurePaths,
  response_model: v.responseModel,
  responses: {
    state: v.state,
    ttl: v.ttl.trim(),
    max_stored_bytes: v.maxStored.trim(),
    max_chain_depth: Number(v.maxDepth),
  },
  reasoning_replay: { enabled: v.replay, ttl: v.replayTtl.trim(), max_entry_bytes: v.replayMax.trim() },
  media_fetch: {
    enabled: v.mediaFetch,
    max_total_bytes: v.mediaTotal.trim(),
    max_objects: Number(v.mediaObjects),
  },
  images: { max_n: Number(v.imagesMax) },
});

function checkBehaviour(v: BehaviourValues): FieldErrors {
  const e: FieldErrors = {};
  const int = (key: keyof BehaviourValues, min: number, max: number) => {
    const raw = String(v[key]);
    const n = Number(raw);
    if (!/^\d+$/.test(raw) || n < min || n > max)
      e[key] = max === Number.MAX_SAFE_INTEGER ? `Enter at least ${min}` : `Enter ${min} to ${max}`;
  };
  for (const key of ['ttl', 'replayTtl'] as const)
    if (!DURATION_RE.test(v[key].trim())) e[key] = 'Enter a duration such as 30d or 24h';
  for (const key of ['maxStored', 'replayMax', 'mediaTotal'] as const)
    if (!SIZE_RE.test(v[key].trim())) e[key] = 'Enter a size such as 32MiB';
  int('maxDepth', 1, Number.MAX_SAFE_INTEGER);
  int('mediaObjects', 1, 256);
  int('imagesMax', 1, 128);
  return e;
}

function BehaviourForm({ initial }: { initial: BehaviourValues }) {
  const saveApi = useGatewaySettings((s) => s.saveApi);
  const canEdit = useCan('patchApiSettings');
  const form = useSettingsForm({
    initial,
    toSpec: behaviourSpec,
    save: saveApi,
    saved: 'API settings saved',
    check: checkBehaviour,
    paths: {
      'api.provider_mounts': 'providerMounts',
      'api.azure_paths': 'azurePaths',
      'api.response_model': 'responseModel',
      'api.responses.state': 'state',
      'api.responses.ttl': 'ttl',
      'api.responses.max_stored_bytes': 'maxStored',
      'api.responses.max_chain_depth': 'maxDepth',
      'api.reasoning_replay.ttl': 'replayTtl',
      'api.reasoning_replay.max_entry_bytes': 'replayMax',
      'api.media_fetch.max_total_bytes': 'mediaTotal',
      'api.media_fetch.max_objects': 'mediaObjects',
      'api.images.max_n': 'imagesMax',
    },
  });
  const { values: v, set, errors } = form;
  const disabled = !canEdit;
  return (
    <SettingsForm form={form} canEdit={canEdit} onSubmit={() => void form.submit()}>
      <Group title="Compatibility">
        <Checkbox
          checked={v.providerMounts}
          disabled={disabled}
          onChange={(on) => set('providerMounts', on)}
          label="Accept paths with a provider prefix"
          description="Such as /openai/v1/chat/completions or /anthropic/v1/messages, for SDKs that add one."
        />
        <Checkbox
          checked={v.azurePaths}
          disabled={disabled}
          onChange={(on) => set('azurePaths', on)}
          label="Accept Azure OpenAI paths"
          description="Such as /openai/deployments/{name}/chat/completions, with the model taken from the path."
        />
        <Field label="Model name in responses" error={errors.responseModel} className="max-w-96">
          <Select
            value={v.responseModel}
            disabled={disabled}
            onChange={(x) => set('responseModel', x)}
            options={[
              { value: 'requested', label: 'The model the app asked for' },
              { value: 'upstream', label: 'The model the provider reports' },
            ]}
          />
        </Field>
      </Group>
      <Group
        title="Stored responses"
        note="For the Responses API's previous_response_id and stored responses."
      >
        <div className="grid grid-cols-2 gap-4 max-md:grid-cols-1">
          <Field label="Who keeps them" error={errors.state}>
            <Select
              value={v.state}
              disabled={disabled}
              onChange={(x) => set('state', x)}
              options={[
                {
                  value: 'gateway',
                  label: 'The gateway',
                  description: 'Works with every provider.',
                },
                {
                  value: 'native_only',
                  label: 'Providers with their own Responses API',
                  description: 'Requests that need stored state go only to them.',
                },
              ]}
            />
          </Field>
          <Field label="Kept for" error={errors.ttl} hint={`Default ${AD.responses.ttl}`}>
            <Input mono value={v.ttl} disabled={disabled} onChange={(e) => set('ttl', e.target.value)} />
          </Field>
          <Field
            label="Largest stored response"
            error={errors.maxStored}
            hint={`Default ${AD.responses.max_stored_bytes}`}
          >
            <Input
              mono
              value={v.maxStored}
              disabled={disabled}
              onChange={(e) => set('maxStored', e.target.value)}
            />
          </Field>
          <Field
            label="Longest chain of responses"
            error={errors.maxDepth}
            hint={`Default ${AD.responses.max_chain_depth}`}
          >
            <NumberInput
              value={v.maxDepth}
              min={1}
              disabled={disabled}
              onChange={(x) => set('maxDepth', x)}
            />
          </Field>
        </div>
      </Group>
      <Group
        title="Reasoning between turns"
        note="When a response is translated between APIs, its reasoning is kept so a later turn that drops it gets it back."
      >
        <Switch
          checked={v.replay}
          disabled={disabled}
          onChange={(on) => set('replay', on)}
          label="Keep reasoning for later turns"
        />
        <div className="grid grid-cols-2 gap-4 max-md:grid-cols-1">
          <Field label="Kept for" error={errors.replayTtl} hint={`Default ${AD.reasoning_replay.ttl}`}>
            <Input
              mono
              value={v.replayTtl}
              disabled={disabled || !v.replay}
              onChange={(e) => set('replayTtl', e.target.value)}
            />
          </Field>
          <Field
            label="Largest entry"
            error={errors.replayMax}
            hint={`Default ${AD.reasoning_replay.max_entry_bytes}`}
          >
            <Input
              mono
              value={v.replayMax}
              disabled={disabled || !v.replay}
              onChange={(e) => set('replayMax', e.target.value)}
            />
          </Field>
        </div>
      </Group>
      <Group title="Media in requests">
        <Switch
          checked={v.mediaFetch}
          disabled={disabled}
          onChange={(on) => set('mediaFetch', on)}
          label="Download image and file URLs for providers that can't"
        />
        <div className="grid grid-cols-3 gap-4 max-md:grid-cols-1">
          <Field
            label="All downloads per request"
            error={errors.mediaTotal}
            hint={`Default ${AD.media_fetch.max_total_bytes}`}
          >
            <Input
              mono
              value={v.mediaTotal}
              disabled={disabled || !v.mediaFetch}
              onChange={(e) => set('mediaTotal', e.target.value)}
            />
          </Field>
          <Field
            label="Downloads per request"
            error={errors.mediaObjects}
            hint={`1 to 256, default ${AD.media_fetch.max_objects}`}
          >
            <NumberInput
              value={v.mediaObjects}
              min={1}
              max={256}
              disabled={disabled || !v.mediaFetch}
              onChange={(x) => set('mediaObjects', x)}
            />
          </Field>
          <Field
            label="Images per request"
            error={errors.imagesMax}
            hint={`1 to 128, default ${AD.images.max_n}`}
          >
            <NumberInput
              value={v.imagesMax}
              min={1}
              max={128}
              disabled={disabled}
              onChange={(x) => set('imagesMax', x)}
            />
          </Field>
        </div>
      </Group>
    </SettingsForm>
  );
}

const SD = SETTINGS_DEFAULTS.streaming;
type StreamingSpec = { max_event_bytes?: string; send_buffer?: string; client_write_timeout?: string };
interface StreamingValues {
  maxEvent: string;
  sendBuffer: string;
  writeTimeout: string;
}

const streamingValues = (spec: StreamingSpec): StreamingValues => ({
  maxEvent: spec.max_event_bytes ?? SD.max_event_bytes,
  sendBuffer: spec.send_buffer ?? SD.send_buffer,
  writeTimeout: spec.client_write_timeout ?? SD.client_write_timeout,
});

const streamingSpec = (v: StreamingValues) => ({
  max_event_bytes: v.maxEvent.trim(),
  send_buffer: v.sendBuffer.trim(),
  client_write_timeout: v.writeTimeout.trim(),
});

function StreamingForm({ initial }: { initial: StreamingValues }) {
  const saveStreaming = useGatewaySettings((s) => s.saveStreaming);
  const canEdit = useCan('patchStreamingSettings');
  const form = useSettingsForm({
    initial,
    toSpec: streamingSpec,
    save: saveStreaming,
    saved: 'Streaming settings saved',
    paths: {
      'streaming.max_event_bytes': 'maxEvent',
      'streaming.send_buffer': 'sendBuffer',
      'streaming.client_write_timeout': 'writeTimeout',
    },
    check: (v) => {
      const e: FieldErrors = {};
      if (!SIZE_RE.test(v.maxEvent.trim())) e.maxEvent = 'Enter a size from 64KiB to 64MiB';
      if (!SIZE_RE.test(v.sendBuffer.trim())) e.sendBuffer = 'Enter a size from 64KiB to 16MiB';
      if (!DURATION_RE.test(v.writeTimeout.trim())) e.writeTimeout = 'Enter a duration from 1s to 300s';
      return e;
    },
  });
  const { values: v, set, errors } = form;
  const disabled = !canEdit;
  return (
    <SettingsForm form={form} canEdit={canEdit} onSubmit={() => void form.submit()}>
      <div className="grid grid-cols-3 gap-4 max-md:grid-cols-1">
        <Field
          label="Largest event"
          error={errors.maxEvent}
          hint={`64KiB to 64MiB, default ${SD.max_event_bytes}`}
        >
          <Input
            mono
            value={v.maxEvent}
            disabled={disabled}
            onChange={(e) => set('maxEvent', e.target.value)}
          />
        </Field>
        <Field
          label="Buffer per stream"
          error={errors.sendBuffer}
          hint={`64KiB to 16MiB, default ${SD.send_buffer}`}
        >
          <Input
            mono
            value={v.sendBuffer}
            disabled={disabled}
            onChange={(e) => set('sendBuffer', e.target.value)}
          />
        </Field>
        <Field
          label="Give up on a slow app after"
          error={errors.writeTimeout}
          hint={`1s to 300s, default ${SD.client_write_timeout}`}
        >
          <Input
            mono
            value={v.writeTimeout}
            disabled={disabled}
            onChange={(e) => set('writeTimeout', e.target.value)}
          />
        </Field>
      </div>
    </SettingsForm>
  );
}

/** Which endpoints apps may call, how requests are read and answered, and streaming. */
export function ApiEndpointsSection() {
  const api = useGatewaySettings((s) => s.api);
  const streaming = useGatewaySettings((s) => s.streaming);
  const loadApi = useGatewaySettings((s) => s.loadApi);
  const configHref = useSettingsHref('configuration');
  useEffect(() => {
    void loadApi();
  }, [loadApi]);
  const spec = (api.data?.spec ?? {}) as ApiSpec;
  const mounts = Object.keys(spec.passthrough ?? {});
  return (
    <>
      {api.status === 'loading' && !api.data ? (
        <Card title="Endpoints">
          <LoadingRows rows={6} />
        </Card>
      ) : null}
      <LoadError of={api} subject="API settings" onRetry={() => void loadApi()} />
      {api.data ? (
        <>
          <EndpointsCard spec={spec} />
          <Card title="Requests and responses" description="How the gateway reads requests and answers them.">
            <BehaviourForm key={api.data.metadata.etag} initial={behaviourValues(spec)} />
          </Card>
          <Card
            title="Passthrough paths"
            description="Paths sent to a provider as they are, without translation."
          >
            <Facts
              items={[
                {
                  label: 'Paths',
                  value: mounts.length ? (
                    <span className="mono">{mounts.join(', ')}</span>
                  ) : (
                    <span className="text-muted">None</span>
                  ),
                  hint: (
                    <>
                      Add or change them in{' '}
                      <Link href={configHref} className="text-accent hover:underline">
                        Configuration
                      </Link>
                      .
                    </>
                  ),
                },
              ]}
            />
          </Card>
        </>
      ) : null}
      <Card title="Streaming" description="Limits for streamed responses.">
        {streaming.status === 'loading' && !streaming.data ? <LoadingRows rows={2} /> : null}
        <LoadError of={streaming} subject="Streaming settings" size="sm" />
        {streaming.data ? (
          <StreamingForm key={streaming.data.metadata.etag} initial={streamingValues(streaming.data.spec)} />
        ) : null}
      </Card>
    </>
  );
}
