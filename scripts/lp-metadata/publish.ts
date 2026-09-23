/**
 * Name, image and pin the metadata for a market's three LP mints.
 *
 * Order matters for the caller: the mints and the market address must exist
 * (they are written into the JSON and the pin keyvalues), and the returned
 * URIs go into `initialize_lp_metadata`, which the program lets a market call
 * once per mint. Everything is also written under `target/lp-metadata/<market>`
 * so the result can be inspected before or after it is pinned.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Connection, PublicKey } from "@solana/web3.js";

import type { LpTokenNaming, MarketLpMintKind } from "../../packages/dusk-sdk/dist/index.js";
import { MARKET_LP_MINT_KINDS } from "../../packages/dusk-sdk/dist/index.js";
import { renderLpImages, type LpImageSources } from "./images.ts";
import { fetchAssetLogo, fetchImage } from "./logos.ts";
import { pinFile, pinJson, type PinataConfig } from "./pinata.ts";

export interface PublishLpMetadataParams {
  connection: Connection;
  network: string;
  market: PublicKey;
  baseMint: PublicKey;
  quoteMint: PublicKey;
  baseSymbol: string;
  quoteSymbol: string;
  mints: Record<MarketLpMintKind, PublicKey>;
  naming: Record<MarketLpMintKind, LpTokenNaming>;
  /** Omit to render and write files without pinning anything. */
  pinata?: PinataConfig;
  /** Override logo sources, e.g. for devnet mocks whose metadata has no image. */
  logoUrls?: { base?: string; quote?: string };
  outDir?: string;
}

export interface PublishedLpMetadata {
  image: string;
  uri: string;
  json: Record<string, unknown>;
}

const KIND_LABEL: Record<MarketLpMintKind, string> = { ylp: "yLP", baseHlp: "hLP", quoteHlp: "hLP" };

export async function resolveLogos(params: PublishLpMetadataParams): Promise<LpImageSources> {
  const [baseLogo, quoteLogo] = await Promise.all([
    params.logoUrls?.base ? fetchImage(params.logoUrls.base) : fetchAssetLogo(params.connection, params.baseMint),
    params.logoUrls?.quote ? fetchImage(params.logoUrls.quote) : fetchAssetLogo(params.connection, params.quoteMint),
  ]);
  return { baseLogo, quoteLogo, baseSymbol: params.baseSymbol, quoteSymbol: params.quoteSymbol };
}

function metadataJson(params: PublishLpMetadataParams, kind: MarketLpMintKind, image: string): Record<string, unknown> {
  const naming = params.naming[kind];
  const side = kind === "baseHlp" ? "base" : kind === "quoteHlp" ? "quote" : "both";
  return {
    name: naming.name,
    symbol: naming.symbol,
    description: naming.description,
    image,
    external_url: "https://omnipair.fi",
    attributes: [
      { trait_type: "protocol", value: "Omnipair Dusk" },
      { trait_type: "kind", value: KIND_LABEL[kind] },
      { trait_type: "side", value: side },
      { trait_type: "market", value: params.market.toBase58() },
      { trait_type: "base", value: params.baseSymbol },
      { trait_type: "quote", value: params.quoteSymbol },
      { trait_type: "network", value: params.network },
    ],
    properties: { category: "image", files: [{ uri: image, type: "image/png" }] },
  };
}

export async function publishLpMetadata(
  params: PublishLpMetadataParams
): Promise<Record<MarketLpMintKind, PublishedLpMetadata>> {
  const outDir = params.outDir ?? join("target", "lp-metadata", params.market.toBase58());
  mkdirSync(outDir, { recursive: true });
  const images = await renderLpImages(await resolveLogos(params));
  const result = {} as Record<MarketLpMintKind, PublishedLpMetadata>;
  for (const kind of MARKET_LP_MINT_KINDS) {
    const mint = params.mints[kind].toBase58();
    const keyvalues = {
      app: "dusk",
      network: params.network,
      market: params.market.toBase58(),
      mint,
      kind,
    };
    writeFileSync(join(outDir, `${kind}.png`), images[kind]);
    let image = `file://${join(process.cwd(), outDir, `${kind}.png`)}`;
    let uri = "";
    if (params.pinata) {
      const pinned = await pinFile(params.pinata, {
        name: `dusk-lp-${mint}.png`,
        bytes: images[kind],
        contentType: "image/png",
        keyvalues: { ...keyvalues, type: "lp-image" },
      });
      image = pinned.url;
    }
    const json = metadataJson(params, kind, image);
    writeFileSync(join(outDir, `${kind}.json`), JSON.stringify(json, null, 2) + "\n");
    if (params.pinata) {
      const pinned = await pinJson(params.pinata, `dusk-lp-${mint}.json`, json, { ...keyvalues, type: "lp-metadata" });
      uri = pinned.url;
    }
    result[kind] = { image, uri, json };
  }
  return result;
}
