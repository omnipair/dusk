/**
 * Where an asset's symbol and logo come from: its Metaplex metadata account,
 * then the JSON that account points at. Nothing here is required — a market
 * can be named and imaged from placeholders — but real logos make the LP
 * tokens recognisable.
 */
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { Connection, PublicKey } from "@solana/web3.js";

export const TOKEN_METADATA_PROGRAM_ID = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

const MAX_LOGO_BYTES = 8 * 1024 * 1024;
const FETCH_TIMEOUT_MS = 20_000;

export interface TokenIdentity {
  mint: PublicKey;
  name: string | null;
  symbol: string | null;
  uri: string | null;
}

export function deriveTokenMetadataAddress(mint: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("metadata"), TOKEN_METADATA_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    TOKEN_METADATA_PROGRAM_ID
  )[0];
}

function readBorshString(data: Buffer, offset: number): [string, number] {
  const length = data.readUInt32LE(offset);
  const start = offset + 4;
  const raw = data.subarray(start, start + length).toString("utf8");
  return [raw.replace(/\0+$/g, "").trim(), start + length];
}

/** Name, symbol and URI from the Metaplex metadata account, or nulls. */
export async function readTokenMetadata(connection: Connection, mint: PublicKey): Promise<TokenIdentity> {
  const account = await connection.getAccountInfo(deriveTokenMetadataAddress(mint), "confirmed");
  if (!account || account.data.length < 1 + 32 + 32 + 4) {
    return { mint, name: null, symbol: null, uri: null };
  }
  try {
    // key(1) + update_authority(32) + mint(32) + name + symbol + uri
    let offset = 1 + 32 + 32;
    const [name, afterName] = readBorshString(account.data, offset);
    offset = afterName;
    const [symbol, afterSymbol] = readBorshString(account.data, offset);
    offset = afterSymbol;
    const [uri] = readBorshString(account.data, offset);
    return { mint, name: name || null, symbol: symbol || null, uri: uri || null };
  } catch {
    return { mint, name: null, symbol: null, uri: null };
  }
}

export function toHttpUrl(uri: string, ipfsGateway = "https://ipfs.io/ipfs/"): string {
  if (uri.startsWith("ipfs://")) return `${ipfsGateway}${uri.slice("ipfs://".length).replace(/^ipfs\//, "")}`;
  if (uri.startsWith("ar://")) return `https://arweave.net/${uri.slice("ar://".length)}`;
  return uri;
}

async function fetchWithTimeout(url: string): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  try {
    return await fetch(url, { signal: controller.signal, redirect: "follow" });
  } finally {
    clearTimeout(timer);
  }
}

/** The image bytes behind a metadata URI, or null when anything is missing. */
export async function fetchTokenLogo(uri: string | null): Promise<Uint8Array | null> {
  if (!uri) return null;
  try {
    const metadataResponse = await fetchWithTimeout(toHttpUrl(uri));
    if (!metadataResponse.ok) return null;
    const contentType = metadataResponse.headers.get("content-type") ?? "";
    let imageUrl: string;
    if (contentType.startsWith("image/")) {
      imageUrl = toHttpUrl(uri);
    } else {
      const json = (await metadataResponse.json()) as { image?: unknown };
      if (typeof json.image !== "string" || json.image.length === 0) return null;
      imageUrl = toHttpUrl(json.image);
    }
    return fetchImage(imageUrl);
  } catch {
    return null;
  }
}

/**
 * The logo for an asset mint, trying the sources a wallet would: the
 * Metaplex metadata JSON, then Jupiter's token registry, then the legacy
 * Solana token list. Well-known mints such as USDC carry no Metaplex metadata
 * at all, which is why the fallbacks exist.
 */
export async function fetchAssetLogo(connection: Connection, mint: PublicKey): Promise<Uint8Array | null> {
  const metadata = await readTokenMetadata(connection, mint);
  const fromMetadata = await fetchTokenLogo(metadata.uri);
  if (fromMetadata) return fromMetadata;
  const registry = await jupiterLogoUrl(mint);
  if (registry) {
    const fromRegistry = await fetchImage(registry);
    if (fromRegistry) return fromRegistry;
  }
  return fetchImage(
    `https://raw.githubusercontent.com/solana-labs/token-list/main/assets/mainnet/${mint.toBase58()}/logo.png`
  );
}

async function jupiterLogoUrl(mint: PublicKey): Promise<string | null> {
  try {
    const response = await fetchWithTimeout(
      `https://lite-api.jup.ag/tokens/v2/search?query=${mint.toBase58()}`
    );
    if (!response.ok) return null;
    const results = (await response.json()) as Array<{ id?: string; icon?: string }>;
    const match = results.find((entry) => entry.id === mint.toBase58());
    return typeof match?.icon === "string" && match.icon.length > 0 ? match.icon : null;
  } catch {
    return null;
  }
}

export async function fetchImage(url: string): Promise<Uint8Array | null> {
  try {
    if (url.startsWith("file://")) {
      const bytes = new Uint8Array(await readFile(fileURLToPath(url)));
      return bytes.length > 0 && bytes.length <= MAX_LOGO_BYTES ? bytes : null;
    }
    const response = await fetchWithTimeout(toHttpUrl(url));
    if (!response.ok) return null;
    const bytes = new Uint8Array(await response.arrayBuffer());
    return bytes.length > 0 && bytes.length <= MAX_LOGO_BYTES ? bytes : null;
  } catch {
    return null;
  }
}
