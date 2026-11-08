/** Standalone pinning uses PINATA_JWT. Without it, LP metadata is generated
 * by the app's constrained pair API; scripts never request upload credentials. */
import { PinataSDK } from "pinata";

export interface PinataConfig {
  jwt?: string;
  metadataEndpoint?: string;
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
    metadataEndpoint: env.LP_METADATA_ENDPOINT || (env.PINATA_JWT ? undefined : "https://beta.omnipair.fi/api/lp-metadata"),
    gateway: env.PINATA_GATEWAY_URL || DEFAULT_PINATA_GATEWAY,
  };
  return config;
}

function gatewayDomain(gateway: string): string {
  return gateway.replace(/^https?:\/\//, "").replace(/\/.*$/, "");
}

function client(config: PinataConfig): PinataSDK {
  return new PinataSDK({ pinataJwt: config.jwt ?? "", pinataGateway: gatewayDomain(config.gateway) });
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
  if (!config.jwt) throw new Error("Standalone file uploads require PINATA_JWT; use LP_METADATA_ENDPOINT for generated LP metadata");
  const existing = await findExisting(config, upload);
  if (existing) return existing;
  const pinata = client(config);
  const file = new File([upload.bytes], upload.name, { type: upload.contentType });
  const result = await pinata.upload.public.file(file).name(upload.name).keyvalues(upload.keyvalues);
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
