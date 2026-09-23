/**
 * Render the three LP images for a pair without touching a chain or Pinata.
 *
 *   BASE=META QUOTE=USDC \
 *   BASE_LOGO=https://... QUOTE_LOGO=https://... \
 *   node scripts/lp-metadata/render_preview.ts
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { marketLpTokenNaming } from "../../packages/dusk-sdk/dist/index.js";
import { renderLpImages } from "./images.ts";
import { fetchImage } from "./logos.ts";

const baseSymbol = process.env.BASE ?? "META";
const quoteSymbol = process.env.QUOTE ?? "USDC";
const outDir = process.env.OUT_DIR ?? join("target", "lp-metadata", "preview");
mkdirSync(outDir, { recursive: true });

const [baseLogo, quoteLogo] = await Promise.all([
  process.env.BASE_LOGO ? fetchImage(process.env.BASE_LOGO) : null,
  process.env.QUOTE_LOGO ? fetchImage(process.env.QUOTE_LOGO) : null,
]);
const images = await renderLpImages({ baseLogo, quoteLogo, baseSymbol, quoteSymbol });
const naming = marketLpTokenNaming({ baseSymbol, quoteSymbol });
for (const kind of ["ylp", "baseHlp", "quoteHlp"] as const) {
  const file = join(outDir, `${kind}.png`);
  writeFileSync(file, images[kind]);
  console.log(`${naming[kind].symbol.padEnd(11)} ${naming[kind].name.padEnd(16)} ${file}`);
}
