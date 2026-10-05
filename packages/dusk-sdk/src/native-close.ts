/** The launch size fee is constant within each bucket but jumps at its edge. */
export interface NativeCloseFeeTiers {
  referenceNad: bigint;
  incrementBps: number;
  maxFeeBps: number;
  collateralDecimals: number;
}

export interface NativeCloseSearch {
  maxCollateralIn: bigint;
  feeTiers?: NativeCloseFeeTiers;
  transferFee?: NativeCloseTransferFee;
  /** False means too little repayment; liquidity-limited means the sale is too large. */
  canClose(collateralIn: bigint): Promise<boolean | "liquidity-limited">;
  signal?: AbortSignal;
}

const U64_MAX = 0xffff_ffff_ffff_ffffn;
const NAD_DECIMALS = 9;

export interface NativeCloseTransferFee {
  transferFeeBasisPoints: number;
  maximumFee: bigint;
}

/** Spendable credit for one gross transfer; capped fees apply per transfer. */
export function nativeCloseCredit(gross: bigint, fee?: NativeCloseTransferFee): bigint {
  if (!fee) return gross;
  const proportional = ceilDiv(gross * BigInt(fee.transferFeeBasisPoints), 10_000n);
  return gross - (proportional < fee.maximumFee ? proportional : fee.maximumFee);
}

/** Least gross transfer that delivers the required net amount, including rounding. */
export function nativeCloseGrossForCredit(net: bigint, fee?: NativeCloseTransferFee): bigint {
  if (!fee || net === 0n) return net;
  let low = net - 1n;
  let high = net + fee.maximumFee < U64_MAX ? net + fee.maximumFee : U64_MAX;
  if (nativeCloseCredit(high, fee) < net) throw new Error("Transfer fee makes the required credit unreachable");
  while (high - low > 1n) {
    const middle = low + (high - low) / 2n;
    if (nativeCloseCredit(middle, fee) >= net) high = middle;
    else low = middle;
  }
  return high;
}

function ceilDiv(numerator: bigint, denominator: bigint): bigint {
  return (numerator + denominator - 1n) / denominator;
}

/**
 * Find the least sufficient raw collateral input. The caller's predicate must
 * evaluate the executable close at the current market state. A plain binary
 * search is invalid while launch size fees introduce downward output jumps.
 */
export async function findMinimumNativeCloseCollateralIn({
  maxCollateralIn,
  feeTiers,
  transferFee,
  canClose,
  signal,
}: NativeCloseSearch): Promise<bigint> {
  if (maxCollateralIn <= 0n || maxCollateralIn > U64_MAX)
    throw new Error("maxCollateralIn must be a positive u64 amount");
  if (transferFee && (!Number.isInteger(transferFee.transferFeeBasisPoints) ||
    transferFee.transferFeeBasisPoints < 0 || transferFee.transferFeeBasisPoints > 10_000 ||
    transferFee.maximumFee < 0n || transferFee.maximumFee > U64_MAX))
    throw new Error("Invalid collateral transfer fee");
  const probe = async (amount: bigint): Promise<boolean | "liquidity-limited"> => {
    signal?.throwIfAborted();
    return amount === 0n ? false : canClose(amount);
  };
  // A size-limited quote can bound the search from above, but must never be
  // returned as executable; the selected amount needs a successful simulation.
  const coversDebt = (result: boolean | "liquidity-limited") => result !== false;
  let low = 0n;
  let high = maxCollateralIn;
  const maximumCoversDebt = coversDebt(await probe(maxCollateralIn));
  if (feeTiers && feeTiers.maxFeeBps > 0) {
    const { referenceNad, incrementBps, maxFeeBps, collateralDecimals } = feeTiers;
    if (referenceNad <= 0n || incrementBps <= 0 || !Number.isInteger(collateralDecimals) || collateralDecimals < 0)
      throw new Error("Invalid launch size-fee configuration");
    const scale = 10n ** BigInt(Math.abs(collateralDecimals - NAD_DECIMALS));
    const maximumCredit = nativeCloseCredit(maxCollateralIn, transferFee);
    const maximumNad = collateralDecimals > NAD_DECIMALS
      ? ceilDiv(maximumCredit, scale)
      : maximumCredit * scale;
    const maximumBucket = ceilDiv(maximumNad, referenceNad);
    const capBucket = 1n + ceilDiv(BigInt(maxFeeBps), BigInt(incrementBps));
    const lastBucket = maximumBucket < capBucket ? maximumBucket : capBucket;
    const endpoint = (bucket: bigint): bigint => {
      const limitNad = bucket * referenceNad;
      const creditLimit = collateralDecimals > NAD_DECIMALS ? limitNad * scale : limitNad / scale;
      // The launch fee tier is selected using reserve credit, while the user
      // specifies gross custody debit. Include a possible transfer-fee plateau.
      return creditLimit >= maximumCredit ? maxCollateralIn
        : nativeCloseGrossForCredit(creditLimit + 1n, transferFee) - 1n;
    };
    let lowBucket = 0n;
    let highBucket = lastBucket;
    if (!maximumCoversDebt) {
      // The maximum may sit just above a fee jump while an earlier tier's
      // endpoint still repays debt. The old on-chain search rejected here.
      if (lastBucket <= 1n) throw new Error("Collateral cap cannot repay the current debt");
      highBucket = lastBucket - 1n;
      high = endpoint(highBucket);
      if (!coversDebt(await probe(high))) throw new Error("Collateral cap cannot repay the current debt");
    }
    while (highBucket - lowBucket > 1n) {
      const middle = lowBucket + (highBucket - lowBucket) / 2n;
      const amount = endpoint(middle);
      if (coversDebt(await probe(amount))) {
        highBucket = middle;
        high = amount;
      } else {
        lowBucket = middle;
        low = amount;
      }
    }
  } else if (!maximumCoversDebt) {
    throw new Error("Collateral cap cannot repay the current debt");
  }
  while (high - low > 1n) {
    const middle = low + (high - low) / 2n;
    if (coversDebt(await probe(middle))) high = middle;
    else low = middle;
  }
  if ((await probe(high)) !== true)
    throw new Error("No executable collateral sale can repay the current debt");
  return high;
}
