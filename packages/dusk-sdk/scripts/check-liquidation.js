import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair, ComputeBudgetProgram, SystemProgram } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, createTransferCheckedInstruction, calculateFee } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DuskLiquidations } from "../dist/liquidation-client.js";
import { liquidationHealth, liquidationIncentives, liquidationFeeAllocation, liquidationInsuranceTarget, liquidationLossAllocation, liquidationDistressAge, grossLiquidationPayment, deriveLiquidationSessionAddress, deriveLiquidationPaymentAddress } from "../dist/liquidation.js";

test("time raises the reward without turning 6% equity into emergency permission", () => {
  const health = { collateralValue: 1_000_000n, debt: 940_000n, maintenanceBps: 700 };
  assert.equal(liquidationHealth(health).eligible, true);
  assert.equal(liquidationIncentives(health, 120n).emergencyAllowed, false);
  const boundary = { ...health, debt: 951_000n };
  assert.deepEqual([0n, 60n, 120n].map(age => liquidationIncentives(boundary, age).emergencyRewardBps), [30, 65, 100]);
  assert.equal(liquidationIncentives({ ...health, debt: 1_000_001n }, 0n).emergencyRewardBps, 100);
  assert.throws(() => liquidationDistressAge({ active: true, startedAt: 121n }, 120n));
  assert.equal(liquidationDistressAge({ active: false, startedAt: 0n }, 1000n), 0n);
});

test("one percent liquidation fee follows the 80/20 split and insurance taper", () => {
  assert.equal(liquidationInsuranceTarget(100_000n, 30_000n), 7_000n);
  assert.deepEqual(liquidationFeeAllocation(100_000n, 8_750n, 10_000n), { total: 1000n, protocol: 200n, insurance: 400n, lp: 400n });
  assert.deepEqual(liquidationFeeAllocation(100_000n, 10_001n, 10_000n, 50n), { total: 50n, protocol: 10n, insurance: 0n, lp: 40n });
});

test("insolvent liquidation cancels interest and cannot draw more than 75% of principal loss", () => {
  assert.deepEqual(liquidationLossAllocation(900n, 950n, 850n, 1000n), {
    principalRepaid: 850n, interestPaid: 0n, insuranceCredit: 37n, principalWrittenOff: 13n, interestCanceled: 50n, surplus: 0n,
  });
  assert.equal(liquidationLossAllocation(900n, 950n, 850n, 10n).principalWrittenOff, 40n);
  assert.throws(() => liquidationLossAllocation(900n, 950n, 850n, 1000n, 7501));
});

test("incoming gross-up is minimal under the actual SPL transfer-fee formula", () => {
  for (const bps of [0, 1, 100, 9999, 10000]) for (const cap of [0n, 1n, 10n, 1000n]) for (const net of [0n, 1n, 99n, 100n, 9999n]) {
    const fee = { epoch: 0n, transferFeeBasisPoints: bps, maximumFee: cap };
    const gross = grossLiquidationPayment(net, { basisPoints: bps, maximumFee: cap });
    assert.equal(gross - calculateFee(fee, gross), net);
    if (gross > 0n) assert.ok(gross - 1n - calculateFee(fee, gross - 1n) < net);
  }
});

test("atomic builder binds the exact settle index, custom program and owner-owned payout", async () => {
  const keys = Array.from({ length: 14 }, (_, i) => Keypair.fromSeed(new Uint8Array(32).fill(i + 1)).publicKey);
  const [programId, market, position, owner, buyer, baseMint, quoteMint, baseReserve, quoteReserve, baseCollateral, quoteCollateral, baseInterest, quoteInterest, insurance] = keys;
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID }) }, wallet: { publicKey: buyer }, publicKey: buyer,
  });
  program.account.market.fetch = async () => ({
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, collateralVault: baseCollateral, interestVault: baseInterest },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, collateralVault: quoteCollateral, interestVault: quoteInterest },
    insurance: { baseVault: insurance, quoteVault: insurance },
  });
  program.account.leveragePosition.fetch = async () => ({ market, owner, debtAsset: 1, referralPartner: SystemProgram.programId });
  const client = new DuskLiquidations(program);
  client.preview = async () => ({ payment: new anchor.BN(1002), collateralCredit: new anchor.BN(1100) });
  const request = { market, position, kind: "leverage", debtAsset: "quote" };
  const result = await client.buildFlash({
    ...request, buyer, maxRepayment: 1000n,
    prefixInstructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 })],
    route: ctx => [createTransferCheckedInstruction(ctx.buyerDebtAccount, ctx.debtMint, ctx.repaymentVault, buyer, ctx.requiredPaymentCredit, 6)],
  });
  assert.equal(result.beginIndex, 5);
  assert.equal(result.settleIndex, 7);
  const decoded = program.coder.instruction.decode(result.beginInstruction.data);
  assert.equal(decoded.name, "beginFlashLiquidation");
  assert.equal(decoded.data.args.settleIndex, result.settleIndex);
  assert.equal(decoded.data.args.maxPayment.toString(), "1002");
  assert.equal(program.coder.instruction.decode(result.settleInstruction.data).name, "settleFlashLiquidation");
  assert.ok(result.session.equals(deriveLiquidationSessionAddress(position, programId)[0]));
  assert.ok(result.repaymentVault.equals(deriveLiquidationPaymentAddress(position, quoteMint, programId)[0]));
  const common = program.idl.instructions.find(i => i.name === "beginFlashLiquidation").accounts[0].accounts;
  const ownerMeta = result.beginInstruction.keys[common.findIndex(i => i.name === "ownerDebtAccount")];
  const buyerMeta = result.beginInstruction.keys[common.findIndex(i => i.name === "buyerRefundAccount")];
  assert.ok(!ownerMeta.pubkey.equals(buyerMeta.pubkey));
  client.write.hlpRemainingAccounts = async () => [];
  const internal = await client.buildFlash({ ...request, buyer, maxRepayment: 1000n, settlement: "duskAmm" });
  assert.equal(internal.requiredPaymentCredit, 0n);
  assert.equal(internal.settleIndex, internal.beginIndex + 1);
  assert.ok(internal.settleInstruction.keys.some(meta => meta.pubkey.equals(baseReserve)));
  const customRemaining = { pubkey: quoteCollateral, isSigner: false, isWritable: true };
  const composed = await client.buildFlash({ ...request, buyer, maxRepayment: 1000n,
    settlement: "duskAmm", settleRemainingAccounts: [customRemaining] });
  assert.deepEqual(composed.settleInstruction.keys.at(-1), customRemaining);
  assert.equal(composed.instructions.filter(ix => ix.programId.equals(ComputeBudgetProgram.programId)
    && ix.data[0] === 2).length, 1);
  client.previewEmergency = async () => ({ rewardCredit: new anchor.BN(3), swapOutput: new anchor.BN(900) });
  const emergency = await client.buildEmergency({ ...request, caller: buyer, collateralDebit: 1100n, full: true });
  const emergencyDecoded = program.coder.instruction.decode(emergency.instruction.data);
  assert.equal(emergencyDecoded.name, "emergencyLiquidation");
  assert.equal(emergencyDecoded.data.args.minRewardCredit.toString(), "3");
  assert.equal(emergencyDecoded.data.args.minSwapOutput.toString(), "900");
  assert.ok(!emergency.ownerDebtAccount.equals(emergency.callerDebtAccount));
  await assert.rejects(client.buildFlash({ ...request, buyer, maxRepayment: 1000n }), /explicit payment route/);
  const observe = await client.observeInstruction(request);
  assert.equal(program.coder.instruction.decode(observe.data).name, "observeLiquidation");
  assert.ok(!result.instructions.includes(observe));
});
