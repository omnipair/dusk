import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair, TransactionInstruction } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";
import { leverageActionMetrics, leverageRepaymentQuote, leverageCloseSlice, leverageClosePercentage,
  planLeverageIncrease, planLeverageDecrease, planLeverageRepayAll } from "../dist/leverage-actions.js";
import { previewLeverageAction } from "../dist/leverage-preview.js";

const state = { collateralAmount: 600n, collateralValue: 600n, debtAmount: 400n };
const quote = async input => input;

test("linked leverage defaults to the whole position, not the new slice", async () => {
  const added = await planLeverageIncrease({ state, marginCredit: 100n, maxBorrowAmount: 1000n, quoteCollateralCredit: quote });
  assert.equal(added.debtAmount, 200n);
  assert.equal(added.after.leverageBps, 30_000n);
  const four = await planLeverageIncrease({ state, marginCredit: 100n, maxBorrowAmount: 1000n,
    targetLeverageBps: 40_000n, quoteCollateralCredit: quote });
  assert.equal(four.debtAmount, 500n); // Added slice is 6x; whole position is 4x.
  assert.equal(four.after.leverageBps, 40_000n);
  const size = await planLeverageIncrease({ state, marginCredit: 100n, maxBorrowAmount: 1000n,
    additionalCollateral: 400n, quoteCollateralCredit: quote });
  assert.equal(size.debtAmount, 300n);
  assert.equal(size.collateralCredit, 400n);
  assert.equal(size.requiresSimulation, true);
});

test("fee-aware purchase sizing does not promise the no-fee target", async () => {
  const result = await planLeverageIncrease({ state, marginCredit: 100n, maxBorrowAmount: 1000n,
    targetLeverageBps: 40_000n, quoteCollateralCredit: async input => input * 997n / 1000n });
  assert.ok(result.debtAmount < 500n);
  assert.ok(result.after.leverageBps <= 40_000n);
  await assert.rejects(planLeverageIncrease({ state, marginCredit: 0n, maxBorrowAmount: 1000n,
    targetLeverageBps: 20_000n, quoteCollateralCredit: quote }), /more margin/);
});

test("linked size amounts handle different token decimals without floating-point arithmetic", async () => {
  const result = await planLeverageIncrease({
    state: { collateralAmount: 600_000_000_000n, collateralValue: 600_000_000n, debtAmount: 400_000_000n },
    marginCredit: 100_000_000n, maxBorrowAmount: 1_000_000_000n, quoteCollateralCredit: async net => net * 1000n,
  });
  assert.equal(result.debtAmount, 200_000_000n);
  assert.equal(result.collateralCredit, 300_000_000_000n);
});

test("repayment uses aggregate rounding and repay-all bounds the refund", async () => {
  const repayment = leverageRepaymentQuote({ positionShares: 1n, aggregateShares: 2n, borrowIndexNad: 1_500_000_000n, maxCredit: 2n });
  assert.equal(repayment.debtReduced, 1n);
  assert.equal(repayment.fullRepayCredit, 2n);
  assert.equal(repayment.debtRemaining, 0n);
  assert.throws(() => leverageRepaymentQuote({ positionShares: 1n, aggregateShares: 2n, borrowIndexNad: 1_500_000_000n, maxCredit: 1n }), /granularity/);
  const full = await planLeverageRepayAll({ collateralAmount: 600n, maxCollateralDebit: 599n,
    fullRepayCredit: 401n, quoteDebtOutput: async amount => amount * 2n });
  assert.deepEqual(full, { collateralAmount: 201n, minRepayOut: 401n, maxSurplusOut: 1n, requiresSimulation: true });
  assert.equal(leverageActionMetrics({ ...state, debtAmount: 0n }).leverageBps, 10_000n);
});

test("partial close rounding cannot increase remaining leverage or silently clear debt", () => {
  const half = leverageCloseSlice({ collateralAmount: 601n, debtShares: 401n,
    aggregateShares: 1000n, borrowIndexNad: 1_000_000_000n, closeBps: 5000 });
  assert.equal(half.collateralDebit, 300n);
  assert.equal(half.sharesToBurn, 201n);
  assert.equal(half.debtRepayment, 201n);
  assert.ok(half.remainingDebtShares * 601n <= 401n * half.remainingCollateral);
  assert.deepEqual(leverageClosePercentage(601n, 300n), { closeBps: 4991, collateralDebit: 299n });
  assert.throws(() => leverageCloseSlice({ collateralAmount: 100n, debtShares: 1n,
    aggregateShares: 1n, borrowIndexNad: 1_000_000_000n, closeBps: 5000 }), /clear all debt/);
});

test("decrease-size target preserves equity while an owner partial close pays it out", async () => {
  const plan = await planLeverageDecrease({ state, targetLeverageBps: 20_000n, maxCollateralDebit: 599n,
    positionShares: 400n, aggregateShares: 400n, borrowIndexNad: 1_000_000_000n, quoteDebtOutput: quote });
  assert.equal(plan.collateralAmount, 200n);
  assert.equal(plan.minRepayOut, 200n);
  assert.equal(plan.maxSurplusOut, 0n);
  assert.equal(plan.after.equity, 200n);
  assert.equal(plan.after.leverageBps, 20_000n);
});

test("SDK builders encode owner partial closes, net-risk funding and guarded refunds", async () => {
  const [market, owner, positionId, debtMint, collateralMint, ownerDebtAccount] = Array.from({ length: 6 }, () => Keypair.generate().publicKey);
  const program = new anchor.Program(IDL, { connection: {}, publicKey: owner, wallet: { publicKey: owner } });
  program.account.market.fetch = async () => ({ baseHlpVault: { hlpSupply: 0n, residualExposure: 0n }, quoteHlpVault: { hlpSupply: 0n, residualExposure: 0n } });
  const write = new DuskWrite(program);
  const common = { market, positionOwner: owner, positionId, debtMint, collateralMint, debtAsset: "quote", ownerDebtAccount };
  const decode = ix => program.coder.instruction.decode(ix.data);
  const added = decode(await write.addToLeverageInstruction({ ...common, marginAmount: 100n, debtAmount: 200n, minCollateralOut: 290n }));
  assert.equal(added.name, "increaseLeverage"); assert.equal(added.data.args.marginAmount.toString(), "100");
  const increased = decode(await write.increaseLeverageInstruction({ ...common, debtAmount: 200n, minCollateralOut: 190n }));
  assert.equal(increased.data.args.marginAmount.toString(), "0");
  const decreased = decode(await write.decreaseLeverageInstruction({ ...common, collateralAmount: 201n, minRepayOut: 401n, maxSurplusOut: 1n }));
  assert.equal(decreased.data.args.maxSurplusOut.toString(), "1");
  const closed = decode(await write.closeLeverageInstruction({ ...common, closeBps: 5000, minAmountOut: 99n }));
  assert.equal(closed.data.args.closeBps, 5000);
  const full = decode(await write.closeLeverageInstruction({ ...common, minAmountOut: 0n }));
  assert.equal(full.data.args.closeBps, 10_000);
  const repaid = await write.addLeverageMarginInstruction({ ...common, amount: 100n });
  assert.equal(decode(repaid).name, "repayLeverage");
  const collateralIndex = program.idl.instructions.find(ix => ix.name === "repayLeverage").accounts.findIndex(a => a.name === "collateralMint");
  assert.ok(repaid.keys[collateralIndex].pubkey.equals(collateralMint));
  assert.equal(typeof write.removeLeverageMarginInstruction, "undefined");
  assert.equal(program.idl.instructions.some(ix => ix.name === "removeLeverageMargin"), false);
});

test("actual-action preview reads one bank and reports net wallet flow from CPI events", async () => {
  const [programId, marketKey, positionKey, owner, mint, otherMint] = Array.from({ length: 6 }, () => Keypair.generate().publicKey);
  const market = { baseSide: { assetMint: otherMint, assetDecimals: 6 }, quoteSide: { assetMint: mint, assetDecimals: 6 },
    risk: { basePriceEmaNad: 1_000_000_000n }, debt: { quoteBorrowIndexNad: 1_000_000_000n } };
  const position = { market: marketKey, debtAsset: 1, collateralAmount: 900n, debtShares: 600n,
    marginTerms: { referenceCollateral: 100_000n, admissionEquityCollateralNad: 0n,
      maintenanceBoundariesBps: [500, 1500], maintenanceRatesBps: [700, 1200, 1700], entryBufferBps: 300 } };
  const eventData = Buffer.concat([Buffer.from("e445a52e51cb9a1d", "hex"), Buffer.alloc(8)]);
  let includeEvents = true;
  const program = { programId, coder: {
    accounts: { decode: name => name === "market" ? market : position },
    events: { decode: () => ({ name: "leveragePositionUpdated", data: { market: marketKey, position: positionKey,
      debtAssetMint: mint, ownerDebit: 100n, ownerCredit: 0n } }) },
  }, provider: { publicKey: owner, connection: { simulateTransaction: async (_wire, options) => {
    assert.equal(options.innerInstructions, true);
    assert.equal(options.sigVerify, false);
    assert.deepEqual(options.accounts.addresses, [marketKey.toBase58(), positionKey.toBase58()]);
    return { context: { slot: 42 }, value: { err: null, logs: [], unitsConsumed: 123,
      accounts: [0, 1].map(() => ({ owner: programId.toBase58(), lamports: 100, data: ["", "base64"] })),
      innerInstructions: includeEvents ? [{ index: 2, instructions: [{ programId, data: anchor.utils.bytes.bs58.encode(eventData) }] }] : [],
    } };
  } } } };
  const args = { market: marketKey, leveragePosition: positionKey, instructions: [new TransactionInstruction({ programId, keys: [], data: Buffer.alloc(0) })] };
  const preview = await previewLeverageAction(program, args);
  assert.equal(preview.slot, 42); assert.equal(preview.metrics.leverageBps, 30_000n);
  assert.equal(preview.marginTerms.maintenanceMarginBps, 700);
  assert.deepEqual(preview.walletFlows, [{ mint, debit: 100n, credit: 0n }]);
  includeEvents = false;
  await assert.rejects(previewLeverageAction(program, args), /CPI events/);
});
