/**
 * Uploads to Pinata, the same account and dedicated gateway
 * (`ipfs.omnipair.fi`) the V1 app publishes LP metadata to.
 *
 * Two ways to authenticate. `PINATA_JWT` uploads directly and can look for an
 * existing pin first. `PINATA_SIGNED_URL_ENDPOINT` points at a running webapp's
 * `/api/pinata/create-signed-url` route, which holds the JWT server-side and
 * hands out short-lived upload URLs; that keeps the key out of scripts and out
 * of shells.
 */
import { PinataSDK } from "pinata";

export interface PinataConfig {
  jwt?: string;
  signedUrlEndpoint?: string;
  /** Gateway URL or bare domain; defaults to https://ipfs.omnipair.fi */
  gateway: string;
}

export interface PinataUpload {
  name: string;
  bytes: Uint8Array;
  contentType: string;
  keyvalues: Record<string, string>;
}

export interface PinnedFile {
  cid: string;
  url: string;
  reused: boolean;
}

export const DEFAULT_PINATA_GATEWAY = "https://ipfs.omnipair.fi";

export function pinataConfigFromEnv(env: NodeJS.ProcessEnv = process.env): PinataConfig {
  const config: PinataConfig = {
    jwt: env.PINATA_JWT || undefined,
    signedUrlEndpoint: env.PINATA_SIGNED_URL_ENDPOINT || undefined,
    gateway: env.PINATA_GATEWAY_URL || DEFAULT_PINATA_GATEWAY,
  };
  if (!config.jwt && !config.signedUrlEndpoint) {
    throw new Error("set PINATA_JWT or PINATA_SIGNED_URL_ENDPOINT to upload LP metadata");
  }
  return config;
}

function gatewayDomain(gateway: string): string {
  return gateway.replace(/^https?:\/\//, "").replace(/\/.*$/, "");
}

function client(config: PinataConfig): PinataSDK {
  return new PinataSDK({ pinataJwt: config.jwt ?? "", pinataGateway: gatewayDomain(config.gateway) });
}

async function requestSignedUrl(endpoint: string, upload: PinataUpload): Promise<string> {
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ expires: 120, name: upload.name, keyvalues: upload.keyvalues }),
  });
  const body = (await response.json().catch(() => ({}))) as { url?: unknown; error?: unknown };
  if (!response.ok || typeof body.url !== "string") {
    throw new Error(`signed URL endpoint ${response.status}: ${String(body.error ?? "no url")}`);
  }
  return body.url;
}

/** An earlier pin with exactly these keyvalues, when the JWT allows listing. */
async function findExisting(config: PinataConfig, upload: PinataUpload): Promise<PinnedFile | null> {
  if (!config.jwt) return null;
  try {
    const pinata = client(config);
    const listing = await pinata.files.public.list().keyvalues(upload.keyvalues).limit(1);
    const file = listing.files?.[0];
    if (!file?.cid) return null;
    return { cid: file.cid, url: await pinata.gateways.public.convert(file.cid), reused: true };
  } catch {
    return null;
  }
}

export async function pinFile(config: PinataConfig, upload: PinataUpload): Promise<PinnedFile> {
  const existing = await findExisting(config, upload);
  if (existing) return existing;
  const pinata = client(config);
  const file = new File([upload.bytes], upload.name, { type: upload.contentType });
  let builder = pinata.upload.public.file(file).name(upload.name).keyvalues(upload.keyvalues);
  if (!config.jwt) {
    if (!config.signedUrlEndpoint) throw new Error("no Pinata credentials");
    builder = builder.url(await requestSignedUrl(config.signedUrlEndpoint, upload));
  }
  const result = await builder;
  if (!result?.cid) throw new Error(`Pinata returned no CID for ${upload.name}`);
  return { cid: result.cid, url: await pinata.gateways.public.convert(result.cid), reused: false };
}

export async function pinJson(config: PinataConfig, name: string, value: unknown, keyvalues: Record<string, string>): Promise<PinnedFile> {
  return pinFile(config, {
    name,
    bytes: new TextEncoder().encode(JSON.stringify(value, null, 2)),
    contentType: "application/json",
    keyvalues,
  });
}
