import { DuskWrite } from "../write.js";
import { oracleReadAccounts, encodeOraclePolicy } from "../oracle.js";
import { DEFAULT_READONLY_PUBLIC_KEY } from "../address.js";
import { LAUNCH_FEE_DECAY_DISABLED, LAUNCH_RATE_LIMIT_ASSET_DISABLED } from "../parameters.js";
import * as anchor from "@coral-xyz/anchor";
import type { Program } from "@coral-xyz/anchor";
const BN: typeof anchor.BN = Reflect.get(anchor, "BN") ?? Reflect.get(anchor, "default")?.BN;
import {
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  type AccountInfo,
  type RpcResponseAndContext,
  type SimulatedTransactionResponse,
  type TransactionInstruction,
} from "@solana/web3.js";
import {
  calculateEpochFee,
  getTransferFeeConfig,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  unpackMint,
} from "@solana/spl-token";
import { deriveFutarchyAuthorityAddress, deriveMarketAddress, MARKET_LAYOUT_VERSION } from "../constants.js";
import { decodePreviewSwapReturnData, type SwapPreview } from "../preview.js";
import type { Market } from "../type-aliases.js";
import type { Dusk } from "../types_v2.js";
import { simulatePreviewWithContext, type PreviewSimulationOptions } from "../simulation.js";
import type { DuskVirtualBookSnapshot } from "./market.js";
import { projectDuskVirtualBookCurve } from "./projection.js";

type Side = "bids" | "asks";
export type VirtualBookQuoteRequest = { side: Side; amount: bigint };
export type VirtualBookQuote = {
  preview: SwapPreview;
  outputTransferFee: bigint;
  slot: number;
};
export type DuskVirtualBookQuotes = DuskVirtualBookSnapshot & {
  groupingBps: number;
  firstQuoteSlot: number;
  mid: number;
  quotes: Record<Side, VirtualBookQuote[]>;
};
const CLOCK_OWNER = "Sysvar1111111111111111111111111111111111111";
const BATCH_SIZE = 4;
const U64_MAX = (1n << 64n) - 1n;
const NAD = 1_000_000_000n;
/** Matches the stream clients' first-to-last quote slot limit. */
const MAX_CLOCK_SPREAD_SLOTS = 8;
/** One part per million: 0.01 bps, a five-hundredth of the finest grouping. */
const MAX_CLOCK_DRIFT_NAD = 1_000n;
const raw = (value: { toString(): string }) => {
  const amount = BigInt(value.toString());
  if (amount < 0n || amount > U64_MAX) throw new Error("Invalid depth amount");
  return amount;
};
const canonicalBytes = (data: string[]) => {
  if (
    data.length !== 2 ||
    data[1] !== "base64" ||
    Buffer.from(data[0], "base64").toString("base64") !== data[0]
  )
    throw new Error("Invalid native depth encoding");
  return Buffer.from(data[0], "base64");
};

/** Only the requested top-level, read-only SDK previews can supply these returns. */
export function decodeDuskVirtualBookBatch(
  program: Program<Dusk>,
  snapshot: DuskVirtualBookSnapshot,
  requests: VirtualBookQuoteRequest[],
  result: RpcResponseAndContext<SimulatedTransactionResponse>,
  floor: number
) {
  const { slot } = result.context;
  const { value } = result;
  const programId = snapshot.programId;
  if (program.programId.toBase58() !== programId) throw new Error("Depth SDK deployment mismatch");
  if (!Number.isSafeInteger(slot) || slot < floor || value.err)
    throw new Error("Native market depth preview unavailable");
  const returned = value.returnData;
  const accounts = value.accounts;
  const oracleAccounts = oracleReadAccounts(snapshot.account.oracle.policy);
  if (!returned || returned.programId !== programId || accounts?.length !== 5 + oracleAccounts.length)
    throw new Error("Incomplete native market depth preview");
  const infos: AccountInfo<Buffer>[] = accounts.map((info) => {
    if (!info || info.executable) throw new Error("Invalid native depth account");
    return {
      owner: new PublicKey(info.owner),
      executable: false,
      lamports: info.lamports,
      data: canonicalBytes(info.data),
    };
  });
  const [marketInfo, baseInfo, quoteInfo, authorityInfo, clock] = infos;
  if (
    marketInfo.owner.toBase58() !== programId ||
    authorityInfo.owner.toBase58() !== programId ||
    clock.owner.toBase58() !== CLOCK_OWNER ||
    clock.data.length !== 40 ||
    clock.data.readBigUInt64LE(0) !== BigInt(slot)
  )
    throw new Error("Native depth account identity or clock mismatch");
  const epoch = clock.data.readBigUInt64LE(16);
  const market = program.coder.accounts.decode<Market>("market", marketInfo.data);
  const [address, bump] = deriveMarketAddress(
    market.baseSide.assetMint,
    market.quoteSide.assetMint,
    market.paramsHash,
    program.programId
  );
  if (
    market.version !== MARKET_LAYOUT_VERSION ||
    market.bump !== bump ||
    address.toBase58() !== snapshot.market ||
    !market.baseSide.assetMint.equals(snapshot.account.baseSide.assetMint) ||
    !market.quoteSide.assetMint.equals(snapshot.account.quoteSide.assetMint) ||
    market.amm.concentratedCurveCache.mathRevision !== 1
  )
    throw new Error("Native depth market identity changed");
  if (!Buffer.from(encodeOraclePolicy(market.oracle.policy))
    .equals(Buffer.from(encodeOraclePolicy(snapshot.account.oracle.policy))))
    throw new Error("Depth oracle policy changed");
  for (const info of infos.slice(5)) {
    if (!info.owner.equals(market.oracle.policy.provider))
      throw new Error("Depth oracle owner changed");
  }
  const mint = (info: AccountInfo<Buffer>, side: Market["baseSide"]) => {
    if (!info.owner.equals(TOKEN_PROGRAM_ID) && !info.owner.equals(TOKEN_2022_PROGRAM_ID))
      throw new Error("Invalid depth token program");
    const decoded = unpackMint(side.assetMint, info, info.owner);
    if (!decoded.isInitialized || decoded.decimals !== side.assetDecimals)
      throw new Error("Depth token precision changed");
    return decoded;
  };
  const base = mint(baseInfo, market.baseSide);
  const quote = mint(quoteInfo, market.quoteSide);
  const prefix = `Program return: ${programId} `;
  const returns = (value.logs ?? [])
    .filter((line) => line.startsWith(prefix))
    .map((line) => line.slice(prefix.length));
  if (returns.length !== requests.length || returns.at(-1) !== returned.data[0])
    throw new Error("Incomplete native depth return log");
  canonicalBytes(returned.data);
  const quotes = returns.map((encoded, index) => {
    canonicalBytes([encoded, "base64"]);
    const preview = decodePreviewSwapReturnData([encoded, "base64"]);
    const request = requests[index];
    const fromBase = request.side === "bids";
    if (
      Object.keys(preview.assetIn).join() !== (fromBase ? "base" : "quote") ||
      Object.keys(preview.assetOut).join() !== (fromBase ? "quote" : "base") ||
      raw(preview.exactAssetIn) !== request.amount ||
      raw(preview.startPriceNad) === 0n ||
      raw(preview.totalFeeRateNad) > 1_000_000_000n ||
      raw(preview.divergenceFeeRateNad) + raw(preview.volatilityFeeRateNad) >
        raw(preview.totalFeeRateNad)
    )
      throw new Error("Native depth quote does not match its request");
    // SwapPreview.amountOut is the vault debit. The real swap applies the
    // output mint's transfer fee once more before crediting the user's account.
    const transferFees = getTransferFeeConfig(fromBase ? quote : base);
    const outputTransferFee = transferFees
      ? calculateEpochFee(transferFees, epoch, raw(preview.amountOut))
      : 0n;
    return { side: request.side, preview, outputTransferFee, slot };
  });
  return {
    market,
    quotes,
    slot,
    epoch,
    unixTimestamp: clock.data.readBigInt64LE(32),
    // Every source account except Clock: market, mints, authority and oracle
    // feeds. Equal bytes mean no transaction touched any quote input between
    // batches. The Clock is compared on its own in combineVirtualBookBatches,
    // because parallel simulations reach confirmed banks a few slots apart.
    state: infos
      .filter((_, index) => index !== 4)
      .map((info) => `${info.owner}:${info.data.toString("base64")}`)
      .join("|"),
  };
}

/** One read-only native preview for each requested cumulative input. */
export async function previewVirtualBookBatch(
  program: Program<Dusk>,
  snapshot: DuskVirtualBookSnapshot,
  requests: VirtualBookQuoteRequest[],
  options: PreviewSimulationOptions = {}
) {
  if (program.programId.toBase58() !== snapshot.programId)
    throw new Error("Depth SDK deployment mismatch");
  if (!requests.length || requests.length > BATCH_SIZE)
    throw new Error("Depth batches require one to four requests");
  const { account, market } = snapshot;
  const floor = Math.max(snapshot.slot, options.minContextSlot ?? 0);
  const instructions = await Promise.all(
    requests.map(({ side, amount }) => {
      if (side !== "bids" && side !== "asks") throw new Error("Invalid depth side");
      if (raw(amount) === 0n) throw new Error("Invalid depth amount");
      const fromBase = side === "bids";
      return new DuskWrite(program).instruction("previewSwap", { exactAssetIn: new BN(amount.toString()) }, {
        accounts: {
          market: new PublicKey(market),
          futarchyAuthority: deriveFutarchyAuthorityAddress(program.programId)[0],
          assetInMint: fromBase ? account.baseSide.assetMint : account.quoteSide.assetMint,
          assetOutMint: fromBase ? account.quoteSide.assetMint : account.baseSide.assetMint,
        },
        oraclePolicy: account.oracle.policy,
      });
    })
  );
  const response = await simulatePreviewWithContext(program, instructions, {
    ...options,
    feePayer: options.feePayer ?? DEFAULT_READONLY_PUBLIC_KEY,
    commitment: "confirmed",
    minContextSlot: floor,
    accounts: [
      market,
      account.baseSide.assetMint,
      account.quoteSide.assetMint,
      deriveFutarchyAuthorityAddress(program.programId)[0],
      SYSVAR_CLOCK_PUBKEY,
      ...oracleReadAccounts(account.oracle.policy).map(meta => meta.pubkey),
    ],
  });
  return {
    ...decodeDuskVirtualBookBatch(program, snapshot, requests, response, floor),
    observedAt: response.observedAt,
  };
}

export interface VirtualBookQuoteOptions extends PreviewSimulationOptions {
  groupingBps?: number;
  /** Price levels a side, 1 to 50; twelve by default. Every level is one
   * native swap preview, so a deeper book costs that many more. */
  levels?: number;
}

/** Sample the curve, then price cumulative sizes through bounded native batches. */
export async function previewVirtualBookQuotes(
  program: Program<Dusk>,
  snapshot: DuskVirtualBookSnapshot,
  options: VirtualBookQuoteOptions = {}
): Promise<DuskVirtualBookQuotes> {
  options.signal?.throwIfAborted();
  const groupingBps = options.groupingBps ?? 10;
  if (program.programId.toBase58() !== snapshot.programId)
    throw new Error("Depth SDK deployment mismatch");
  const curve = projectDuskVirtualBookCurve(snapshot, groupingBps, options.levels ?? 12);
  if (!curve) throw new Error("Market depth curve unavailable");
  const requests: VirtualBookQuoteRequest[] = [];
  for (const side of ["bids", "asks"] as const) {
    let previous = 0n;
    const decimals =
      side === "bids"
        ? snapshot.account.baseSide.assetDecimals
        : snapshot.account.quoteSide.assetDecimals;
    for (const level of curve[side]) {
      const decimal = (side === "bids" ? level.total : level.quoteTotal).toFixed(decimals);
      const [whole, fraction = ""] = decimal.split(".");
      if (!/^(0|[1-9][0-9]*)(\.[0-9]*)?$/.test(decimal)) throw new Error("Invalid depth amount");
      const amount = raw(
        BigInt(whole) * 10n ** BigInt(decimals) + BigInt(fraction.padEnd(decimals, "0") || "0")
      );
      if (amount <= previous) continue;
      requests.push({ side, amount });
      previous = amount;
    }
  }
  const batches: VirtualBookQuoteRequest[][] = [];
  const batchSize = snapshot.account.oracle.policy.lambdaBps > 0 ? 1 : BATCH_SIZE;
  for (let i = 0; i < requests.length; i += batchSize)
    batches.push(requests.slice(i, i + batchSize));
  const results = await Promise.all(
    batches.map((batch) => previewVirtualBookBatch(program, snapshot, batch, options))
  );
  options.signal?.throwIfAborted();
  return combineVirtualBookBatches(snapshot, groupingBps, curve.mid, results);
}

/** Refuse to combine observations across trades, fee changes, epochs or stale banks.
 *
 * Batches may come from confirmed banks up to eight slots apart when no
 * transaction touched their inputs. Within one epoch, a swap preview reads the
 * Clock for interest, EMA and volatility decay (at a fixed 200 ms per slot,
 * half-lives of at least one minute), the concentration ramp, recentring gates
 * and the Unix-timestamp launch fees. Combining then requires everything a
 * Clock change could move to agree: no active ramp, launch windows over unless
 * the timestamps match, one centre, and start prices and volatility surcharges
 * within one part per million.
 */
export function combineVirtualBookBatches(
  snapshot: DuskVirtualBookSnapshot,
  groupingBps: number,
  emptyMid: number,
  results: Awaited<ReturnType<typeof previewVirtualBookBatch>>[]
): DuskVirtualBookQuotes {
  const first = results[0];
  const lastSlot = Math.max(snapshot.slot, ...results.map((result) => result.slot));
  const firstQuoteSlot = results.length
    ? Math.min(...results.map((result) => result.slot))
    : snapshot.slot;
  if (
    lastSlot - firstQuoteSlot > MAX_CLOCK_SPREAD_SLOTS ||
    results.some(
      (result) =>
        result.slot < snapshot.slot || result.state !== first.state || result.epoch !== first.epoch
    )
  )
    throw new Error("Market depth changed during native previews");
  if (
    results.some(
      (result) => result.slot !== first.slot || result.unixTimestamp !== first.unixTimestamp
    )
  ) {
    // A ramp applies a new curve candidate each slot. Launch fees and the launch
    // size limit step with whole Unix seconds until their windows end.
    const { amm, config } = first.market;
    const fees = config.amm;
    const feeWindow =
      fees.launchFeeDecayMode !== LAUNCH_FEE_DECAY_DISABLED || fees.launchMarketPriceStepBps > 0
        ? BigInt(fees.launchFeeDurationSeconds.toString())
        : 0n;
    const limitWindow =
      fees.launchRateLimitAsset !== LAUNCH_RATE_LIMIT_ASSET_DISABLED
        ? BigInt(fees.launchRateLimitDurationSeconds.toString())
        : 0n;
    const launchEnd =
      BigInt(config.startTime.toString()) + (feeWindow > limitWindow ? feeWindow : limitWindow);
    if (
      amm.concentrationRamp.active ||
      (results.some((result) => result.unixTimestamp !== first.unixTimestamp) &&
        results.some((result) => result.unixTimestamp < launchEnd))
    )
      throw new Error("Market depth changed during native previews");
  }
  const allQuotes = results.flatMap((result) => result.quotes);
  const start = allQuotes[0]?.preview.startPriceNad.toString();
  const center = allQuotes[0]?.preview.centerPriceNad.toString();
  const bounds = (values: bigint[]) => [
    values.reduce((a, b) => (a < b ? a : b), values[0] ?? 0n),
    values.reduce((a, b) => (a > b ? a : b), 0n),
  ];
  const [lowStart, highStart] = bounds(allQuotes.map((quote) => raw(quote.preview.startPriceNad)));
  const [lowVolatility, highVolatility] = bounds(
    allQuotes.map((quote) => raw(quote.preview.decayedVolatilityNad))
  );
  const [, volatilityRate] = bounds(
    allQuotes.map((quote) => raw(quote.preview.volatilityFeeRateNad))
  );
  // Interest accrual moves the reserves, and with them the start price, by
  // the elapsed slots. The volatility surcharge rate scales at most linearly
  // with decayed volatility. Both stay within one part per million.
  if (
    allQuotes.some((quote) => quote.preview.centerPriceNad.toString() !== center) ||
    (highStart - lowStart) * NAD > MAX_CLOCK_DRIFT_NAD * lowStart ||
    volatilityRate * (highVolatility - lowVolatility) > MAX_CLOCK_DRIFT_NAD * highVolatility
  )
    throw new Error("Market depth price changed during native previews");
  return {
    ...snapshot,
    account: first?.market ?? snapshot.account,
    slot: lastSlot,
    firstQuoteSlot,
    groupingBps,
    mid: start ? Number(start) / 1e9 : emptyMid,
    quotes: {
      bids: allQuotes.filter((quote) => quote.side === "bids"),
      asks: allQuotes.filter((quote) => quote.side === "asks"),
    },
  };
}
