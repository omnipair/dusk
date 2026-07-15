import { PublicKey } from "@solana/web3.js";

import { LEVERAGE_MARGIN_MODE, type LeverageMarginMode } from "./constants.js";

export type LeverageMarketAsset = 0 | 1;

export interface LeverageRoute {
  debtAsset: LeverageMarketAsset;
  collateralAsset: LeverageMarketAsset;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  marginMode: LeverageMarginMode;
}

export interface LeverageTransferFee {
  basisPoints: number;
  maximumFee: bigint | string;
}

export function deriveLeverageRoute(args: {
  baseMint: PublicKey;
  quoteMint: PublicKey;
  longAssetMint: PublicKey;
  fundingMint: PublicKey;
}): LeverageRoute {
  const { baseMint, quoteMint, longAssetMint, fundingMint } = args;
  if (baseMint.equals(quoteMint)) {
    throw new Error("Leverage market mints must be distinct");
  }

  let collateralAsset: LeverageMarketAsset;
  if (longAssetMint.equals(baseMint)) collateralAsset = 0;
  else if (longAssetMint.equals(quoteMint)) collateralAsset = 1;
  else throw new Error("Long asset is not part of the leverage market");
  const debtAsset: LeverageMarketAsset = collateralAsset === 0 ? 1 : 0;
  const collateralMint = collateralAsset === 0 ? baseMint : quoteMint;
  const debtMint = debtAsset === 0 ? baseMint : quoteMint;
  let marginMode: LeverageMarginMode;
  if (fundingMint.equals(debtMint)) {
    marginMode = LEVERAGE_MARGIN_MODE.DEBT;
  } else if (fundingMint.equals(collateralMint)) {
    marginMode = LEVERAGE_MARGIN_MODE.COLLATERAL;
  } else {
    throw new Error("Funding asset is not part of the leverage market");
  }

  return {
    debtAsset,
    collateralAsset,
    debtMint,
    collateralMint,
    marginMode,
  };
}

function ceilDiv(numerator: bigint, denominator: bigint): bigint {
  if (denominator <= 0n) throw new Error("Leverage quote denominator is zero");
  return (numerator + denominator - 1n) / denominator;
}

function validatedTransferFee(config?: LeverageTransferFee): {
  basisPoints: bigint;
  maximumFee: bigint;
} {
  if (!config) return { basisPoints: 0n, maximumFee: 0n };
  if (
    !Number.isInteger(config.basisPoints) ||
    config.basisPoints < 0 ||
    config.basisPoints > 10_000
  ) {
    throw new Error("Invalid token transfer fee");
  }
  const maximumFee = BigInt(config.maximumFee);
  if (maximumFee < 0n) throw new Error("Invalid token transfer fee");
  return {
    basisPoints: BigInt(config.basisPoints),
    maximumFee,
  };
}

export function leverageTransferNetAmount(
  grossAmount: bigint,
  config?: LeverageTransferFee,
): bigint {
  if (grossAmount < 0n) throw new Error("Invalid token transfer amount");
  const { basisPoints, maximumFee } = validatedTransferFee(config);
  if (grossAmount === 0n || basisPoints === 0n) return grossAmount;
  const proportionalFee = ceilDiv(grossAmount * basisPoints, 10_000n);
  const fee = proportionalFee < maximumFee ? proportionalFee : maximumFee;
  return grossAmount > fee ? grossAmount - fee : 0n;
}

export function leverageTransferGrossAmount(
  netAmount: bigint,
  config?: LeverageTransferFee,
): bigint {
  if (netAmount <= 0n) throw new Error("Invalid token transfer amount");
  const { basisPoints, maximumFee } = validatedTransferFee(config);
  if (basisPoints === 0n) return netAmount;
  const inverseFee =
    basisPoints === 10_000n
      ? maximumFee
      : ceilDiv(netAmount * basisPoints, 10_000n - basisPoints);
  return netAmount + (inverseFee < maximumFee ? inverseFee : maximumFee);
}

/**
 * Mirrors the program's exact-output constant-product quote, including its
 * input-denominated swap fee. Token transfer fees must be grossed up separately.
 */
export function quoteLeverageExactOutput(args: {
  reserveIn: bigint;
  reserveOut: bigint;
  amountOut: bigint;
  swapFeeBps: number;
}): bigint {
  const { reserveIn, reserveOut, amountOut } = args;
  if (reserveIn <= 0n || amountOut <= 0n || reserveOut <= amountOut) {
    throw new Error("Insufficient liquidity for leverage quote");
  }
  if (
    !Number.isInteger(args.swapFeeBps) ||
    args.swapFeeBps < 0 ||
    args.swapFeeBps >= 10_000
  ) {
    throw new Error("Invalid leverage swap fee");
  }
  const feeBps = BigInt(args.swapFeeBps);
  const amountInAfterFee = ceilDiv(
    amountOut * reserveIn,
    reserveOut - amountOut,
  );
  return ceilDiv(amountInAfterFee * 10_000n, 10_000n - feeBps);
}

export function quoteLeverageExactInput(args: {
  reserveIn: bigint;
  reserveOut: bigint;
  amountIn: bigint;
  swapFeeBps: number;
}): bigint {
  const { reserveIn, reserveOut, amountIn } = args;
  if (reserveIn <= 0n || reserveOut <= 0n || amountIn <= 0n) {
    throw new Error("Insufficient liquidity for leverage quote");
  }
  if (
    !Number.isInteger(args.swapFeeBps) ||
    args.swapFeeBps < 0 ||
    args.swapFeeBps >= 10_000
  ) {
    throw new Error("Invalid leverage swap fee");
  }
  const fee = ceilDiv(amountIn * BigInt(args.swapFeeBps), 10_000n);
  const amountInAfterFee = amountIn - fee;
  if (amountInAfterFee <= 0n) {
    throw new Error("Insufficient liquidity for leverage quote");
  }
  return (amountInAfterFee * reserveOut) / (reserveIn + amountInAfterFee);
}

export function addLeverageSlippage(amount: bigint, slippageBps = 100): bigint {
  if (amount < 0n || !Number.isInteger(slippageBps) || slippageBps < 0) {
    throw new Error("Invalid leverage slippage");
  }
  return ceilDiv(amount * BigInt(10_000 + slippageBps), 10_000n);
}

export function subtractLeverageSlippage(
  amount: bigint,
  slippageBps = 100,
): bigint {
  if (
    amount < 0n ||
    !Number.isInteger(slippageBps) ||
    slippageBps < 0 ||
    slippageBps > 10_000
  ) {
    throw new Error("Invalid leverage slippage");
  }
  return (amount * BigInt(10_000 - slippageBps)) / 10_000n;
}
