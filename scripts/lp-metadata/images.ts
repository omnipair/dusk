/**
 * LP token images.
 *
 * yLP: the two asset logos, base on the left half and quote on the right,
 * clipped to a circle with a hairline ring — the same composition V1 used for
 * its `omLP` tokens, so the two products read as one family in a wallet.
 *
 * hLP: the logo of the asset the token is exposed to, full circle, with an
 * `h` badge in the lower right so the two hLP mints of a market are told
 * apart from each other and from the yLP at a glance.
 *
 * Missing logos fall back to a two-letter placeholder tile, as V1 did.
 */
import sharp from "sharp";

export interface LpImageSources {
  baseLogo: Uint8Array | null;
  quoteLogo: Uint8Array | null;
  baseSymbol: string;
  quoteSymbol: string;
}

export interface LpImageSet {
  ylp: Buffer;
  baseHlp: Buffer;
  quoteHlp: Buffer;
}

export const DEFAULT_LP_IMAGE_SIZE = 512;

const PLACEHOLDER_FILLS = ["#334155", "#1E3A8A", "#3F3F46", "#4C1D95", "#065F46", "#7C2D12", "#0F172A", "#1F2937"];
const FONT = "Inter, 'Helvetica Neue', Helvetica, Arial, sans-serif";

function hashString(value: string): number {
  let hash = 2166136261;
  for (const char of value) {
    hash ^= char.charCodeAt(0);
    hash = Math.imul(hash, 16777619) >>> 0;
  }
  return hash;
}

function escapeXml(value: string): string {
  return value.replace(/[<>&'"]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", "'": "&apos;", '"': "&quot;" })[c] ?? c);
}

function svg(size: number, body: string): Buffer {
  return Buffer.from(
    `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">${body}</svg>`
  );
}

/** Two-letter tile used when an asset has no logo. */
export function placeholderLogoSvg(symbol: string, size: number): Buffer {
  const initials = escapeXml(symbol.replace(/[^A-Za-z0-9]/g, "").slice(0, 2).toUpperCase() || "?");
  const fill = PLACEHOLDER_FILLS[hashString(symbol) % PLACEHOLDER_FILLS.length];
  return svg(
    size,
    `<rect width="${size}" height="${size}" fill="${fill}"/>` +
      `<text x="50%" y="52%" fill="#ffffff" font-family="${FONT}" font-weight="700" font-size="${size * 0.4}" text-anchor="middle" dominant-baseline="central">${initials}</text>`
  );
}

async function squareLogo(logo: Uint8Array | null, symbol: string, size: number): Promise<Buffer> {
  if (logo) {
    try {
      return await sharp(Buffer.from(logo)).resize(size, size, { fit: "cover" }).ensureAlpha().png().toBuffer();
    } catch {
      // Unreadable image: fall through to the placeholder.
    }
  }
  return sharp(placeholderLogoSvg(symbol, size)).ensureAlpha().png().toBuffer();
}

function circleMask(size: number): Buffer {
  return svg(size, `<circle cx="${size / 2}" cy="${size / 2}" r="${size / 2 - 1}" fill="#ffffff"/>`);
}

function ring(size: number): Buffer {
  const width = Math.max(2, Math.round(size / 64));
  return svg(
    size,
    `<circle cx="${size / 2}" cy="${size / 2}" r="${size / 2 - width}" fill="none" stroke="rgba(255,255,255,0.14)" stroke-width="${width}"/>`
  );
}

function divider(size: number): Buffer {
  const width = Math.max(2, Math.round(size / 128));
  return svg(size, `<rect x="${size / 2 - width / 2}" y="0" width="${width}" height="${size}" fill="rgba(255,255,255,0.35)"/>`);
}

function hedgeBadge(size: number): Buffer {
  const radius = size * 0.19;
  const center = size - radius - size * 0.02;
  const stroke = Math.max(2, Math.round(size / 85));
  return svg(
    size,
    `<circle cx="${center}" cy="${center}" r="${radius}" fill="#0B1220" stroke="rgba(255,255,255,0.9)" stroke-width="${stroke}"/>` +
      `<text x="${center}" y="${center + radius * 0.06}" fill="#ffffff" font-family="${FONT}" font-weight="800" font-size="${radius * 1.45}" text-anchor="middle" dominant-baseline="central">h</text>`
  );
}

async function clipToCircle(image: Buffer, size: number): Promise<Buffer> {
  return sharp(image)
    .ensureAlpha()
    .composite([
      { input: circleMask(size), blend: "dest-in" },
      { input: ring(size), blend: "over" },
    ])
    .png()
    .toBuffer();
}

export async function renderYlpImage(sources: LpImageSources, size = DEFAULT_LP_IMAGE_SIZE): Promise<Buffer> {
  const half = size / 2;
  const [base, quote] = await Promise.all([
    squareLogo(sources.baseLogo, sources.baseSymbol, size),
    squareLogo(sources.quoteLogo, sources.quoteSymbol, size),
  ]);
  const left = await sharp(base).extract({ left: 0, top: 0, width: half, height: size }).png().toBuffer();
  const right = await sharp(quote).extract({ left: half, top: 0, width: half, height: size }).png().toBuffer();
  const joined = await sharp({
    create: { width: size, height: size, channels: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } },
  })
    .composite([
      { input: left, left: 0, top: 0 },
      { input: right, left: half, top: 0 },
      { input: divider(size), blend: "over" },
    ])
    .png()
    .toBuffer();
  return clipToCircle(joined, size);
}

export async function renderHlpImage(
  logo: Uint8Array | null,
  symbol: string,
  size = DEFAULT_LP_IMAGE_SIZE
): Promise<Buffer> {
  const clipped = await clipToCircle(await squareLogo(logo, symbol, size), size);
  return sharp(clipped).composite([{ input: hedgeBadge(size), blend: "over" }]).png().toBuffer();
}

export async function renderLpImages(sources: LpImageSources, size = DEFAULT_LP_IMAGE_SIZE): Promise<LpImageSet> {
  const [ylp, baseHlp, quoteHlp] = await Promise.all([
    renderYlpImage(sources, size),
    renderHlpImage(sources.baseLogo, sources.baseSymbol, size),
    renderHlpImage(sources.quoteLogo, sources.quoteSymbol, size),
  ]);
  return { ylp, baseHlp, quoteHlp };
}
