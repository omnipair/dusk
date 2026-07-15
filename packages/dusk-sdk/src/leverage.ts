import { PublicKey } from "@solana/web3.js";

import {
  LEVERAGE_MARGIN_MODE,
  type LeverageMarginMode,
} from "./constants.js";

export type LeverageMarketAsset = 0 | 1;

export interface LeverageRoute {
  debtAsset: LeverageMarketAsset;
  collateralAsset: LeverageMarketAsset;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  marginMode: LeverageMarginMode;
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
  const feeBps = BigInt(args.swapFeeBps);
  if (reserveIn <= 0n || amountOut <= 0n || reserveOut <= amountOut) {
    throw new Error("Insufficient liquidity for leverage quote");
  }
  if (feeBps < 0n || feeBps >= 10_000n) {
    throw new Error("Invalid leverage swap fee");
  }
  const amountInAfterFee = ceilDiv(
    amountOut * reserveIn,
    reserveOut - amountOut
  );
  return ceilDiv(amountInAfterFee * 10_000n, 10_000n - feeBps);
}

export function addLeverageSlippage(
  amount: bigint,
  slippageBps = 100
): bigint {
  if (amount < 0n || slippageBps < 0) {
    throw new Error("Invalid leverage slippage");
  }
  return ceilDiv(amount * BigInt(10_000 + slippageBps), 10_000n);
}
