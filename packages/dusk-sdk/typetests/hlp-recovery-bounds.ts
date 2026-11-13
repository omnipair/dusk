/**
 * Compile-time checks that the hLP recovery builders have no unbounded
 * defaults: a socialized-loss cap (or a reviewed quote) and an exit payout
 * floor are required.
 *
 * `npm run typecheck` covers this file and never runs it. Each
 * `@ts-expect-error` line must fail to compile; each negative case stays on
 * one line because the directive only covers the line after it.
 */
import type { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import {
  DuskHlpLiquidations,
  type DuskIdl,
  type HlpFlashQuote,
  type HlpLiquidityBuildOptions,
  type HlpRecoveryExit,
} from "../src/index.js";

declare const program: Program<DuskIdl>;
declare const quote: HlpFlashQuote;
declare const exit: HlpRecoveryExit;
declare const options: HlpLiquidityBuildOptions<"withdrawSingleSided">;
const client = new DuskHlpLiquidations(program);
const key = PublicKey.default;
const flash = { market: key, targetAsset: "base" as const, buyer: key, route: () => [] };

void client.buildFlash({ ...flash, maxSocializedLoss: 0n });
void client.buildFlash({ ...flash, quote });
void client.buildFlash({ ...flash, quote, maxSocializedLoss: 10n, allowFull: true, exit });
// @ts-expect-error a flash plan needs maxSocializedLoss or a reviewed quote
void client.buildFlash({ ...flash });

void client.buildNativeCloseAndExit({ market: key, targetAsset: "quote", maxSocializedLoss: 0n, exit });
// @ts-expect-error a native close needs maxSocializedLoss
void client.buildNativeCloseAndExit({ market: key, targetAsset: "quote", exit });

void client.buildNativeCloseAndExit({ market: key, targetAsset: "quote", maxSocializedLoss: 0n, exit: { hlpAmount: 1n, minTargetAmountOut: 0n, options } });
// @ts-expect-error an exit needs minTargetAmountOut
void client.buildNativeCloseAndExit({ market: key, targetAsset: "quote", maxSocializedLoss: 0n, exit: { hlpAmount: 1n, options } });
