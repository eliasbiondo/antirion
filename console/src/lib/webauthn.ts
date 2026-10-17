/**
 * WebAuthn ceremonies in the browser. The gateway sends and expects the JSON forms of the Level 3
 * spec (binary fields as base64url); these helpers convert between them and the browser API.
 */

type Json = Record<string, unknown>;

function fromB64url(text: string): ArrayBuffer {
  const b64 = text
    .replace(/-/g, '+')
    .replace(/_/g, '/')
    .padEnd(Math.ceil(text.length / 4) * 4, '=');
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

function toB64url(buffer: ArrayBuffer | null): string | null {
  if (!buffer) return null;
  let bin = '';
  for (const b of new Uint8Array(buffer)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return list.map((d: { id: string; type?: string; transports?: AuthenticatorTransport[] }) => ({
    id: fromB64url(d.id),
    type: 'public-key',
    ...(d.transports ? { transports: d.transports } : {}),
  }));
}

export function webauthnAvailable(): boolean {
  return (
    typeof window !== 'undefined' &&
    'PublicKeyCredential' in window &&
    typeof navigator.credentials.get === 'function'
  );
}

function credentialJson(cred: PublicKeyCredential): Json {
  const r = cred.response as AuthenticatorAssertionResponse & AuthenticatorAttestationResponse;
  const response: Json = { clientDataJSON: toB64url(r.clientDataJSON) };
  if ('authenticatorData' in r) {
    response.authenticatorData = toB64url(r.authenticatorData);
    response.signature = toB64url(r.signature);
    response.userHandle = toB64url(r.userHandle);
  }
  if ('attestationObject' in r) {
    response.attestationObject = toB64url(r.attestationObject);
    response.transports = typeof r.getTransports === 'function' ? r.getTransports() : [];
  }
  return {
    id: cred.id,
    rawId: toB64url(cred.rawId),
    type: cred.type,
    clientExtensionResults: cred.getClientExtensionResults(),
    response,
  };
}

/** Asks the authenticator for an assertion against the gateway's request options. */
export async function webauthnGet(options: Json): Promise<Json> {
  const publicKey: PublicKeyCredentialRequestOptions = {
    challenge: fromB64url(String(options.challenge)),
    ...(typeof options.rpId === 'string' ? { rpId: options.rpId } : {}),
    ...(typeof options.timeout === 'number' ? { timeout: options.timeout } : {}),
    ...(options.userVerification
      ? { userVerification: options.userVerification as UserVerificationRequirement }
      : {}),
    ...(descriptors(options.allowCredentials)
      ? { allowCredentials: descriptors(options.allowCredentials) }
      : {}),
  };
  const cred = (await navigator.credentials.get({ publicKey })) as PublicKeyCredential | null;
  if (!cred) throw new Error('The security key did not answer.');
  return credentialJson(cred);
}

/** Registers a new credential from the gateway's creation options. */
export async function webauthnCreate(options: Json): Promise<Json> {
  const user = options.user as { id: string; name: string; displayName: string };
  const publicKey: PublicKeyCredentialCreationOptions = {
    challenge: fromB64url(String(options.challenge)),
    rp: options.rp as PublicKeyCredentialRpEntity,
    user: { id: fromB64url(user.id), name: user.name, displayName: user.displayName },
    pubKeyCredParams: options.pubKeyCredParams as PublicKeyCredentialParameters[],
    ...(typeof options.timeout === 'number' ? { timeout: options.timeout } : {}),
    ...(options.authenticatorSelection ? { authenticatorSelection: options.authenticatorSelection } : {}),
    ...(options.attestation ? { attestation: options.attestation as AttestationConveyancePreference } : {}),
    ...(descriptors(options.excludeCredentials)
      ? { excludeCredentials: descriptors(options.excludeCredentials) }
      : {}),
  };
  const cred = (await navigator.credentials.create({ publicKey })) as PublicKeyCredential | null;
  if (!cred) throw new Error('The security key did not answer.');
  return credentialJson(cred);
}
