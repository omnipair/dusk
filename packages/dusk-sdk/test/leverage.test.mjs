import assert from "node:assert/strict";
import test from "node:test";

import { PublicKey } from "@solana/web3.js";

import {
  DUSK_PROGRAM_ID,
  IDL,
  LEVERAGE_MARGIN_MODE,
  SEEDS,
  addLeverageSlippage,
  deriveLeverageCollateralVaultAddress,
  deriveLeveragePositionAddress,
  deriveLeverageRoute,
  leverageTransferGrossAmount,
  leverageTransferNetAmount,
  quoteLeverageExactInput,
  quoteLeverageExactOutput,
  subtractLeverageSlippage,
} from "../dist/index.js";

const key = (byte) => new PublicKey(Buffer.alloc(32, byte));
const BASE_MINT = key(1);
const QUOTE_MINT = key(2);
const OUTSIDE_MINT = key(3);

function ceilDiv(numerator, denominator) {
  return (numerator + denominator - 1n) / denominator;
}

function exactInputOutput({ reserveIn, reserveOut, amountIn, swapFeeBps }) {
  const fee = ceilDiv(amountIn * BigInt(swapFeeBps), 10_000n);
  const amountInAfterFee = amountIn - fee;
  return (amountInAfterFee * reserveOut) / (reserveIn + amountInAfterFee);
}

test("deriveLeverageRoute covers every direction and funding combination", () => {
  const cases = [
    {
      longAssetMint: BASE_MINT,
      fundingMint: QUOTE_MINT,
      debtAsset: 1,
      collateralAsset: 0,
      marginMode: LEVERAGE_MARGIN_MODE.DEBT,
    },
    {
      longAssetMint: BASE_MINT,
      fundingMint: BASE_MINT,
      debtAsset: 1,
      collateralAsset: 0,
      marginMode: LEVERAGE_MARGIN_MODE.COLLATERAL,
    },
    {
      longAssetMint: QUOTE_MINT,
      fundingMint: BASE_MINT,
      debtAsset: 0,
      collateralAsset: 1,
      marginMode: LEVERAGE_MARGIN_MODE.DEBT,
    },
    {
      longAssetMint: QUOTE_MINT,
      fundingMint: QUOTE_MINT,
      debtAsset: 0,
      collateralAsset: 1,
      marginMode: LEVERAGE_MARGIN_MODE.COLLATERAL,
    },
  ];

  for (const expected of cases) {
    const route = deriveLeverageRoute({
      baseMint: BASE_MINT,
      quoteMint: QUOTE_MINT,
      longAssetMint: expected.longAssetMint,
      fundingMint: expected.fundingMint,
    });
    assert.equal(route.debtAsset, expected.debtAsset);
    assert.equal(route.collateralAsset, expected.collateralAsset);
    assert.equal(route.marginMode, expected.marginMode);
    assert(
      route.debtMint.equals(expected.debtAsset === 0 ? BASE_MINT : QUOTE_MINT),
    );
    assert(
      route.collateralMint.equals(
        expected.collateralAsset === 0 ? BASE_MINT : QUOTE_MINT,
      ),
    );
  }
});

test("deriveLeverageRoute rejects malformed markets and assets", () => {
  assert.throws(
    () =>
      deriveLeverageRoute({
        baseMint: BASE_MINT,
        quoteMint: BASE_MINT,
        longAssetMint: BASE_MINT,
        fundingMint: BASE_MINT,
      }),
    /market mints must be distinct/,
  );
  assert.throws(
    () =>
      deriveLeverageRoute({
        baseMint: BASE_MINT,
        quoteMint: QUOTE_MINT,
        longAssetMint: OUTSIDE_MINT,
        fundingMint: BASE_MINT,
      }),
    /Long asset is not part/,
  );
  assert.throws(
    () =>
      deriveLeverageRoute({
        baseMint: BASE_MINT,
        quoteMint: QUOTE_MINT,
        longAssetMint: BASE_MINT,
        fundingMint: OUTSIDE_MINT,
      }),
    /Funding asset is not part/,
  );
});

test("exact-output quotes are conservative and minimal across fee ranges", () => {
  for (const [reserveIn, reserveOut] of [
    [1_000_000n, 2_000_000n],
    [2_000_000n, 1_000_000n],
  ]) {
    for (const swapFeeBps of [0, 1, 30, 500, 9_999]) {
      for (const amountOut of [1n, 2n, 999n, 50_000n]) {
        const amountIn = quoteLeverageExactOutput({
          reserveIn,
          reserveOut,
          amountOut,
          swapFeeBps,
        });
        assert(
          exactInputOutput({
            reserveIn,
            reserveOut,
            amountIn,
            swapFeeBps,
          }) >= amountOut,
        );
        assert(
          exactInputOutput({
            reserveIn,
            reserveOut,
            amountIn: amountIn - 1n,
            swapFeeBps,
          }) < amountOut,
        );
      }
    }
  }
});

test("exact-output and slippage helpers reject invalid inputs", () => {
  const valid = {
    reserveIn: 1_000n,
    reserveOut: 2_000n,
    amountOut: 100n,
    swapFeeBps: 30,
  };

  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, reserveIn: 0n }),
    /Insufficient liquidity/,
  );
  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, amountOut: 0n }),
    /Insufficient liquidity/,
  );
  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, amountOut: 2_000n }),
    /Insufficient liquidity/,
  );
  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, swapFeeBps: -1 }),
    /Invalid leverage swap fee/,
  );
  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, swapFeeBps: 10_000 }),
    /Invalid leverage swap fee/,
  );
  assert.throws(
    () => quoteLeverageExactOutput({ ...valid, swapFeeBps: 30.5 }),
    /Invalid leverage swap fee/,
  );
  assert.equal(addLeverageSlippage(20_471n, 100), 20_676n);
  assert.equal(addLeverageSlippage(0n, 0), 0n);
  assert.throws(() => addLeverageSlippage(-1n), /Invalid leverage slippage/);
  assert.throws(() => addLeverageSlippage(1n, -1), /Invalid leverage slippage/);
  assert.throws(
    () => addLeverageSlippage(1n, 1.5),
    /Invalid leverage slippage/,
  );
});

test("exact-input, transfer-fee, and output-floor helpers mirror program rounding", () => {
  const transferFee = { basisPoints: 100, maximumFee: 10n };
  assert.equal(leverageTransferNetAmount(1_010n, transferFee), 1_000n);
  assert.equal(leverageTransferGrossAmount(1_000n, transferFee), 1_010n);

  const amountOut = quoteLeverageExactInput({
    reserveIn: 1_000_000n,
    reserveOut: 2_000_000n,
    amountIn: 10_000n,
    swapFeeBps: 30,
  });
  assert.equal(
    amountOut,
    exactInputOutput({
      reserveIn: 1_000_000n,
      reserveOut: 2_000_000n,
      amountIn: 10_000n,
      swapFeeBps: 30,
    }),
  );
  assert.equal(subtractLeverageSlippage(10_001n, 100), 9_900n);
  assert.throws(
    () =>
      leverageTransferNetAmount(1n, {
        basisPoints: 10_001,
        maximumFee: 1n,
      }),
    /Invalid token transfer fee/,
  );
});

test("leverage PDAs use the program's canonical position and vault seeds", () => {
  const market = key(4);
  const positionId = key(5);
  const collateralMint = key(6);

  const position = deriveLeveragePositionAddress(market, positionId);
  const expectedPosition = PublicKey.findProgramAddressSync(
    [SEEDS.LEVERAGE_POSITION, market.toBuffer(), positionId.toBuffer()],
    DUSK_PROGRAM_ID,
  );
  assert.equal(position[0].toBase58(), expectedPosition[0].toBase58());
  assert.equal(position[1], expectedPosition[1]);

  const vault = deriveLeverageCollateralVaultAddress(market, collateralMint);
  const expectedVault = PublicKey.findProgramAddressSync(
    [
      SEEDS.LEVERAGE_COLLATERAL_VAULT,
      market.toBuffer(),
      collateralMint.toBuffer(),
    ],
    DUSK_PROGRAM_ID,
  );
  assert.equal(vault[0].toBase58(), expectedVault[0].toBase58());
  assert.equal(vault[1], expectedVault[1]);
});

test("published IDL contains the complete dual-mode leverage interface", () => {
  const instructionNames = new Set(
    IDL.instructions.map((instruction) => instruction.name),
  );
  for (const instruction of [
    "open_leverage",
    "open_collateral_margin_leverage",
    "close_leverage",
    "close_collateral_margin_leverage",
    "delegated_close_leverage",
    "delegated_close_collateral_margin_leverage",
    "increase_leverage",
    "decrease_leverage",
    "add_leverage_margin",
    "remove_leverage_margin",
    "deposit_leverage_collateral",
    "withdraw_leverage_collateral",
    "liquidate_leverage",
    "create_leverage_delegation",
    "update_leverage_delegation",
    "close_leverage_delegation",
  ]) {
    assert(instructionNames.has(instruction), `IDL is missing ${instruction}`);
  }

  const leveragePositionType = IDL.types.find(
    (type) => type.name === "LeveragePosition",
  );
  assert(leveragePositionType, "IDL is missing LeveragePosition");
  assert(
    leveragePositionType.type.fields.some(
      (field) => field.name === "margin_mode",
    ),
    "LeveragePosition is missing margin_mode",
  );
});
