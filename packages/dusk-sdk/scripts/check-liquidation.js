import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair, ComputeBudgetProgram, PublicKey, SystemProgram, AddressLookupTableAccount, TransactionMessage, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TransferFeeConfigLayout, createTransferCheckedInstruction, calculateFee } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DEFAULT_PROTOCOL_REVENUE_SHARE_BPS, MARKET_LAYOUT_VERSION } from "../dist/constants.js";
import { DuskLiquidations } from "../dist/liquidation-client.js";
import { LIQUIDATION_POLICY, liquidationHealth, liquidationIncentives, liquidationFeeAllocation, liquidationLossAllocation, grossLiquidationPayment, deriveLiquidationSessionAddress, deriveLiquidationPaymentAddress } from "../dist/liquidation.js";

test("health alone determines bounded incentives and exact eligibility", () => {
  for (const maintenanceBps of [1, 700, 1200, 1700, 9999]) {
    const collateralValue = 100_000_000n;
    const required = collateralValue * BigInt(maintenanceBps) / 10_000n;
    const h = { collateralValue, debt: collateralValue - required, maintenanceBps };
    assert.equal(liquidationIncentives(h).maximumBuyerDiscountBps, 50);
    assert.equal(liquidationIncentives(h).emergencyRewardBps, 0);
    assert.throws(() => liquidationIncentives({ ...h, debt: h.debt - 1n }));
    const emergency = { ...h, debt: collateralValue - required * 70n / 100n };
    assert.equal(liquidationIncentives(emergency).emergencyAllowed, true);
    assert.equal(liquidationIncentives(emergency).emergencyRewardBps, 30);
    assert.equal(liquidationHealth({ ...emergency, debt: emergency.debt - 1n }).emergencyAllowed, false);
    assert.equal(liquidationIncentives({ ...h, debt: collateralValue - required / 2n }).maximumBuyerDiscountBps, 275);
    for (const debt of [collateralValue, collateralValue + 1n, (1n << 64n) - 1n]) {
      assert.equal(liquidationIncentives({ ...h, debt }).maximumBuyerDiscountBps, 500);
      assert.equal(liquidationIncentives({ ...h, debt }).emergencyRewardBps, 100);
    }
    assert.throws(() => liquidationIncentives({ ...h, debt: 0n }));
  }
  for (const maintenanceBps of [0, 10_000, -1, 1.5]) assert.throws(() => liquidationIncentives({ collateralValue: 100n, debt: 100n, maintenanceBps }));
  assert.equal(liquidationIncentives({ collateralValue: 0n, debt: 1n, maintenanceBps: 700 }).maximumBuyerDiscountBps, 500);
  assert.throws(() => liquidationIncentives({ collateralValue: 0n, debt: 0n, maintenanceBps: 700 }));
});

test("fixed three percent liquidation fee allocates all underwriting income to junior", () => {
  assert.deepEqual(liquidationFeeAllocation(100_000n), { total: 3_000n, protocol: 600n, junior: 2_400n });
  assert.deepEqual(liquidationFeeAllocation(100_000n, 50n), { total: 50n, protocol: 10n, junior: 40n });
  for (const repayment of [0n, 99n, 100n, (1n << 64n) - 1n]) {
    const fee = liquidationFeeAllocation(repayment);
    assert.equal(fee.protocol + fee.junior, fee.total);
  }
});

test("insolvent liquidation cancels interest and passes the whole principal gap to tranches", () => {
  assert.deepEqual(liquidationLossAllocation(900n, 950n, 850n), {
    principalRepaid: 850n, interestPaid: 0n, principalWrittenOff: 50n, interestCanceled: 50n, surplus: 0n,
  });
  assert.equal(liquidationLossAllocation(900n, 950n, 850n).principalWrittenOff, 50n);
  assert.throws(() => liquidationLossAllocation(951n, 950n, 850n));
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
  const [programId, market, position, owner, buyer, baseMint, quoteMint, baseReserve, quoteReserve, baseCollateral, quoteCollateral, baseInterest, quoteInterest, routeProgram] = keys;
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID }) }, wallet: { publicKey: buyer }, publicKey: buyer,
  });
  const inactiveHlp = { hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0) };
  program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION, srlpMint: routeProgram,
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, collateralVault: baseCollateral, interestVault: baseInterest },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, collateralVault: quoteCollateral, interestVault: quoteInterest },
    baseHlpVault: { ...inactiveHlp, srlpVault: owner }, quoteHlpVault: { ...inactiveHlp, srlpVault: position },
  });
  program.account.leveragePosition.fetch = async () => ({ market, owner, debtAsset: 1, referralPartner: SystemProgram.programId });
  const client = new DuskLiquidations(program);
  const flashQuote = { payment: new anchor.BN(1002), collateralCredit: new anchor.BN(1100), rates: { buyerDiscountBps: 455, emergencyRewardBps: 100 } };
  client.preview = async () => flashQuote;
  client.previewAmm = async () => ({ liquidation: flashQuote, swapOutput: new anchor.BN(1200),
    obligation: new anchor.BN(1100), buyerDebit: new anchor.BN(100), buyerCredit: new anchor.BN(97) });
  const request = { market, position, kind: "leverage", debtAsset: "quote" };
  const result = await client.buildFlash({
    ...request, buyer, maxRepayment: 1000n,
    prefixInstructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 })],
    route: ctx => {
      assert.equal(ctx.quote.rates.buyerDiscountBps, 455);
      return [createTransferCheckedInstruction(ctx.buyerDebtAccount, ctx.debtMint, ctx.repaymentVault, buyer, ctx.requiredPaymentCredit, 6)];
    },
  });
  assert.equal(result.beginIndex, 5);
  assert.equal(result.settleIndex, 7);
  const decoded = program.coder.instruction.decode(result.beginInstruction.data);
  assert.equal(decoded.name, "beginFlashLiquidation");
  assert.equal(decoded.data.args.settleIndex, result.settleIndex);
  assert.equal(decoded.data.args.maxPayment.toString(), "1002");
  assert.equal(decoded.data.args.minBuyerCredit.toString(), "0");
  assert.equal(program.coder.instruction.decode(result.settleInstruction.data).name, "settleFlashLiquidation");
  assert.ok(result.session.equals(deriveLiquidationSessionAddress(position, programId)[0]));
  assert.ok(result.repaymentVault.equals(deriveLiquidationPaymentAddress(position, quoteMint, programId)[0]));
  const common = program.idl.instructions.find(i => i.name === "beginFlashLiquidation").accounts[0].accounts;
  const ownerMeta = result.beginInstruction.keys[common.findIndex(i => i.name === "ownerDebtAccount")];
  const buyerMeta = result.beginInstruction.keys[common.findIndex(i => i.name === "buyerRefundAccount")];
  assert.ok(!ownerMeta.pubkey.equals(buyerMeta.pubkey));
  const addresses = [...new Map(result.instructions.flatMap(ix => ix.keys.map(meta =>
    [meta.pubkey.toBase58(), meta.pubkey]))).values()];
  const lookup = new AddressLookupTableAccount({ key: Keypair.generate().publicKey, state: {
    deactivationSlot: (1n << 64n) - 1n, lastExtendedSlot: 0, lastExtendedSlotStartIndex: 0,
    authority: buyer, addresses,
  } });
  let simulated = false;
  program.provider.connection.simulateTransaction = async wire => {
    const message = TransactionMessage.decompile(wire.message, { addressLookupTableAccounts: [lookup] });
    const begin = program.coder.instruction.decode(message.instructions[result.beginIndex].data);
    assert.equal(begin.name, "beginFlashLiquidation");
    assert.equal(begin.data.args.settleIndex, result.settleIndex);
    assert.equal(program.coder.instruction.decode(message.instructions[result.settleIndex].data).name,
      "settleFlashLiquidation");
    for (const tag of [1, 2]) assert.equal(message.instructions.filter(ix =>
      ix.programId.equals(ComputeBudgetProgram.programId) && ix.data[0] === tag).length, 1);
    simulated = true;
    return { context: { slot: 123 }, value: { err: null, logs: [] } };
  };
  await client.get.simulateWithContext(result.instructions, { lookupTables: [lookup], requireReturnData: false });
  assert.ok(simulated);
  await assert.rejects(client.get.simulateWithContext(result.instructions.filter(ix =>
    !ix.programId.equals(ComputeBudgetProgram.programId)), { requireReturnData: false }),
    /requires planned heap and compute budgets/);
  let healthInstruction;
  client.get.simulateReturnData = async instruction => {
    healthInstruction = instruction;
    return { data: [program.coder.types.encode("liquidationHealth", { collateralValue: new anchor.BN(1000), debt: new anchor.BN(940), maintenanceBps: 700 }).toString("base64"), "base64"] };
  };
  const health = await client.previewHealth(request);
  assert.equal(health.debt.toString(), "940");
  assert.equal(program.coder.instruction.decode(healthInstruction.data).name, "previewLiquidationHealth");
  assert.ok(healthInstruction.keys.every(meta => !meta.isWritable && !meta.isSigner));
  client.write.hlpRemainingAccounts = async () => [];
  const internal = await client.buildFlash({ ...request, buyer, maxRepayment: 1000n, settlement: "duskAmm" });
  assert.equal(internal.requiredPaymentCredit, 0n);
  assert.equal(internal.ammQuote.buyerCredit.toString(), "97");
  assert.equal(program.coder.instruction.decode(internal.beginInstruction.data).data.args.minBuyerCredit.toString(), "97");
  assert.equal(internal.settleIndex, internal.beginIndex + 1);
  assert.ok(internal.settleInstruction.keys.some(meta => meta.pubkey.equals(baseReserve)));
  const customRemaining = { pubkey: quoteCollateral, isSigner: false, isWritable: true };
  const composed = await client.buildFlash({ ...request, buyer, maxRepayment: 1000n,
    settlement: "duskAmm", settleRemainingAccounts: [customRemaining] });
  assert.deepEqual(composed.settleInstruction.keys.at(-1), customRemaining);
  assert.equal(composed.instructions.filter(ix => ix.programId.equals(ComputeBudgetProgram.programId)
    && ix.data[0] === 2).length, 1);
  client.previewEmergency = async () => ({ rewardCredit: new anchor.BN(3), swapOutput: new anchor.BN(900),
    referencePriceNad: new anchor.BN(1_000_000_000), referenceDepthNad: new anchor.BN("123456789123456789123456789") });
  const emergency = await client.buildEmergency({ ...request, caller: buyer, collateralDebit: 1100n, full: true });
  const emergencyDecoded = program.coder.instruction.decode(emergency.instruction.data);
  assert.equal(emergencyDecoded.name, "emergencyLiquidation");
  assert.deepEqual(emergency.reference, { priceNad: 1_000_000_000n, depthNad: 123456789123456789123456789n });
  assert.equal(emergencyDecoded.data.args.minRewardCredit.toString(), "3");
  assert.equal(emergencyDecoded.data.args.minSwapOutput.toString(), "900");
  assert.ok(!emergency.ownerDebtAccount.equals(emergency.callerDebtAccount));
  await assert.rejects(client.buildFlash({ ...request, buyer, maxRepayment: 1000n }), /explicit payment route/);
  assert.equal(typeof client.observeInstruction, "undefined");
  assert.ok(!program.idl.instructions.some(i => i.name === "observeLiquidation"));
  assert.ok(!result.instructions.some(ix => ix.programId.equals(programId) && program.coder.instruction.decode(ix.data)?.name === "previewLiquidationHealth"));
});

test("external flash settlement carries the active-hLP prefix by default", async () => {
  const keys = Array.from({ length: 16 }, (_, i) => Keypair.fromSeed(new Uint8Array(32).fill(i + 40)).publicKey);
  const [programId, market, position, owner, buyer, baseMint, quoteMint, baseReserve, quoteReserve, baseCollateral, quoteCollateral, baseInterest, quoteInterest, srlpMint, baseSrlpVault, quoteSrlpVault] = keys;
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID }) }, wallet: { publicKey: buyer }, publicKey: buyer,
  });
  const hlp = (supply, srlpVault) => ({ hlpSupply: new anchor.BN(supply), residualExposure: new anchor.BN(0), srlpVault });
  let baseSupply = 100;
  program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION, srlpMint,
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, collateralVault: baseCollateral, interestVault: baseInterest },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, collateralVault: quoteCollateral, interestVault: quoteInterest },
    baseHlpVault: hlp(baseSupply, baseSrlpVault), quoteHlpVault: hlp(0, quoteSrlpVault),
  });
  program.account.leveragePosition.fetch = async () => ({ market, owner, debtAsset: 1, referralPartner: SystemProgram.programId });
  const client = new DuskLiquidations(program);
  client.preview = async () => ({ payment: new anchor.BN(1002), collateralCredit: new anchor.BN(1100), rates: { buyerDiscountBps: 455, emergencyRewardBps: 100 } });
  const request = { market, position, kind: "leverage", debtAsset: "quote", buyer, maxRepayment: 1000n,
    route: (ctx) => [createTransferCheckedInstruction(ctx.buyerDebtAccount, ctx.debtMint, ctx.repaymentVault, buyer, ctx.requiredPaymentCredit, 6)] };
  const fixed = (function leaves(accounts) {
    return accounts.reduce((count, account) => count + (account.accounts ? leaves(account.accounts) : 1), 0);
  })(program.idl.instructions.find((ix) => ix.name === "settleFlashLiquidation").accounts);
  const prefix = await client.write.hlpRemainingAccounts(market);
  assert.equal(prefix.length, 9);
  assert.deepEqual(prefix.slice(0, 5).map((meta) => meta.pubkey),
    [srlpMint, baseSrlpVault, quoteSrlpVault, baseInterest, quoteInterest]);

  const external = await client.buildFlash(request);
  assert.deepEqual(external.settleInstruction.keys.slice(fixed), prefix);

  // An explicit list still replaces the default exactly.
  const custom = { pubkey: quoteCollateral, isSigner: false, isWritable: true };
  const overridden = await client.buildFlash({ ...request, settleRemainingAccounts: [custom] });
  assert.deepEqual(overridden.settleInstruction.keys.slice(fixed), [custom]);

  // Without an active hLP the settle instruction gains no accounts.
  baseSupply = 0;
  const inactive = await client.buildFlash(request);
  assert.equal(inactive.settleInstruction.keys.length, fixed);
});

test("external flash routes get the gross transfer a fee-bearing debt mint needs", async () => {
  const keys = Array.from({ length: 13 }, (_, i) => Keypair.fromSeed(new Uint8Array(32).fill(i + 80)).publicKey);
  const [programId, market, position, owner, buyer, baseMint, quoteMint, baseReserve, quoteReserve, baseCollateral, quoteCollateral, baseInterest, quoteInterest] = keys;
  // A Token-2022 debt mint charging 2.5% now, with a 90% fee announced for epoch 12.
  const effective = { epoch: 0n, transferFeeBasisPoints: 250, maximumFee: 1_000_000n };
  const announced = { epoch: 12n, transferFeeBasisPoints: 9_000, maximumFee: 1_000_000n };
  const mintData = Buffer.alloc(278);
  mintData[44] = 6; mintData[45] = 1; mintData[165] = 1;
  mintData.writeUInt16LE(1, 166); mintData.writeUInt16LE(108, 168);
  TransferFeeConfigLayout.encode({ transferFeeConfigAuthority: owner, withdrawWithheldAuthority: owner,
    withheldAmount: 0n, olderTransferFee: effective, newerTransferFee: announced }, mintData.subarray(170));
  const clock = Buffer.alloc(40);
  clock.writeBigUInt64LE(500n); clock.writeBigUInt64LE(10n, 16);
  let debtProgram = TOKEN_2022_PROGRAM_ID;
  const feeReads = [];
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: {
      getAccountInfo: async (mint) => ({ owner: mint.equals(quoteMint) ? debtProgram : TOKEN_PROGRAM_ID }),
      getMultipleAccountsInfoAndContext: async (accounts) => {
        feeReads.push(accounts);
        assert.ok(accounts[0].equals(quoteMint) && accounts[1].equals(SYSVAR_CLOCK_PUBKEY));
        return { context: { slot: 500 }, value: [
          { data: mintData, owner: TOKEN_2022_PROGRAM_ID, executable: false, lamports: 1 },
          { data: clock, owner: new PublicKey("Sysvar1111111111111111111111111111111111111"), executable: false, lamports: 1 },
        ] };
      },
    },
    wallet: { publicKey: buyer }, publicKey: buyer,
  });
  const inactiveHlp = { hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0) };
  program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION, srlpMint: owner,
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, collateralVault: baseCollateral, interestVault: baseInterest },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, collateralVault: quoteCollateral, interestVault: quoteInterest },
    baseHlpVault: { ...inactiveHlp, srlpVault: owner }, quoteHlpVault: { ...inactiveHlp, srlpVault: position },
  });
  program.account.leveragePosition.fetch = async () => ({ market, owner, debtAsset: 1, referralPartner: SystemProgram.programId });
  const client = new DuskLiquidations(program);
  const flashQuote = { payment: new anchor.BN(1002), collateralCredit: new anchor.BN(1100) };
  client.preview = async () => flashQuote;
  client.previewAmm = async () => ({ liquidation: flashQuote, buyerCredit: new anchor.BN(97) });
  const request = { market, position, kind: "leverage", debtAsset: "quote", buyer, maxRepayment: 1000n };
  let context;
  const plan = await client.buildFlash({ ...request, route: (ctx) => {
    context = ctx;
    return [createTransferCheckedInstruction(ctx.buyerDebtAccount, ctx.debtMint, ctx.repaymentVault, buyer,
      ctx.requiredPaymentGross, 6, [], ctx.debtTokenProgram)];
  } });
  assert.equal(plan.requiredPaymentCredit, 1002n);
  // ceil(1002 / 0.975) under the current 2.5% fee, not the announced 90% fee.
  assert.equal(plan.requiredPaymentGross, 1028n);
  assert.equal(context.requiredPaymentGross, plan.requiredPaymentGross);
  assert.equal(plan.requiredPaymentGross - calculateFee(effective, plan.requiredPaymentGross), 1002n);
  assert.ok(plan.requiredPaymentGross - 1n - calculateFee(effective, plan.requiredPaymentGross - 1n) < 1002n);
  assert.equal(plan.paymentTransferFee.epoch, 10n);
  assert.equal(plan.paymentTransferFee.phase, "notice");
  assert.equal(plan.paymentTransferFee.effective.transferFeeBasisPoints, 250);
  assert.equal(plan.paymentTransferFee.pending.transferFeeBasisPoints, 9_000);
  assert.equal(feeReads.length, 1);
  // The route's transfer carries the gross amount.
  const routeTransfer = plan.instructions[plan.beginIndex + 1];
  assert.equal(routeTransfer.data.readBigUInt64LE(1), 1028n);

  // Dusk-AMM settlement routes no payment, and a legacy SPL mint charges no fee.
  client.write.hlpRemainingAccounts = async () => [];
  const internal = await client.buildFlash({ ...request, settlement: "duskAmm" });
  assert.equal(internal.requiredPaymentGross, 0n);
  assert.equal(internal.paymentTransferFee, null);
  debtProgram = TOKEN_PROGRAM_ID;
  const legacy = await client.buildFlash({ ...request, route: () => [] });
  assert.equal(legacy.requiredPaymentGross, 1002n);
  assert.equal(legacy.paymentTransferFee, null);
  assert.equal(feeReads.length, 1);
});
