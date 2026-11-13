import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { Keypair, PublicKey } from "@solana/web3.js";

import {
  deriveBorrowPositionAddress,
  deriveEventAuthorityAddress,
  deriveFutarchyAuthorityAddress,
  deriveHlpCohortAddress,
  deriveIncomeEpochAddress,
  deriveJuniorCohortAddress,
  deriveJuniorMintAddress,
  deriveLeverageCollateralVaultAddress,
  deriveLeverageDelegationAddress,
  deriveLeveragePositionAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveReferralAccrualAddress,
  deriveReferralPartnerAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  DUSK_PROGRAM_ID,
  MARKET_LAYOUT_VERSION,
} from "../dist/constants.js";
import { createLeverageDelegateProgram, DuskLeverageOrders } from "../dist/delegate.js";
import { Dusk } from "../dist/dusk.js";
import { DuskGet, pda } from "../dist/get.js";
import { defaultMarketLaunchConfig } from "../dist/market-bootstrap.js";
import { duskIdl } from "../dist/program.js";
import { DuskProtectionOrders, PROTECTION_ACTION } from "../dist/protection.js";
import { DuskTranches } from "../dist/tranche-client.js";
import { DuskWrite } from "../dist/write.js";

const { BN } = anchor;
const key = (seed) => Keypair.fromSeed(new Uint8Array(32).fill(seed)).publicKey;
// A deployment other than DUSK_PROGRAM_ID, such as a fork or a staging build.
const programId = key(90);
const [owner, market, positionId, baseMint, quoteMint, srlpMint, baseHlpMint, quoteHlpMint, referrer, oldHlpMint, newHlpMint] =
  Array.from({ length: 11 }, (_, i) => key(i + 1));

// A legacy SPL mint for every account read, so builders resolve the Token program.
const mintData = Buffer.alloc(82);
mintData[44] = 6;
mintData[45] = 1;
const provider = {
  connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID, data: mintData, lamports: 1, executable: false }) },
  publicKey: owner,
  wallet: { publicKey: owner },
};
const inactiveHlp = (srlpVault) => ({ srlpVault, hlpSupply: new BN(0), residualExposure: new BN(0) });
const book = { epoch: new BN(4), units: new BN(10), unpaid: new BN(0) };
const marketState = {
  version: MARKET_LAYOUT_VERSION,
  srlpMint,
  juniorMint: PublicKey.default,
  baseSide: { assetMint: baseMint, hlpMint: baseHlpMint, reserveVault: key(20), interestVault: key(21) },
  quoteSide: { assetMint: quoteMint, hlpMint: quoteHlpMint, reserveVault: key(22), interestVault: key(23) },
  baseHlpVault: inactiveHlp(key(24)),
  quoteHlpVault: inactiveHlp(key(25)),
  income: { books: [book, book, book, book], pending: [new BN(0), new BN(0), new BN(0), new BN(0)] },
  tranches: { generation: new BN(0), juniorReserveShares: new BN(0) },
};

function customProgram() {
  const program = new anchor.Program(duskIdl(programId), provider);
  program.account.market.fetch = async () => marketState;
  program.account.referralAccrual.fetch = async () => ({ amount: new BN(5) });
  return program;
}

/** Instruction accounts by IDL name, nested composites as `outer.inner`. */
function accountsOf(program, instruction) {
  const decoded = program.coder.instruction.decode(instruction.data);
  const spec = program.idl.instructions.find((candidate) => candidate.name === decoded.name);
  const names = [];
  (function flatten(accounts, prefix) {
    for (const account of accounts) {
      if (account.accounts) flatten(account.accounts, `${prefix}${account.name}.`);
      else names.push(prefix + account.name);
    }
  })(spec.accounts, "");
  return Object.fromEntries(names.map((name, index) => [name, instruction.keys[index].pubkey.toBase58()]));
}

const at = (derived) => derived[0].toBase58();

/** Record the program of every PDA derived while `build` runs. */
async function derivationPrograms(build) {
  const original = PublicKey.findProgramAddressSync;
  const programs = [];
  PublicKey.findProgramAddressSync = function (seeds, owningProgram) {
    programs.push(owningProgram.toBase58());
    return original.call(this, seeds, owningProgram);
  };
  try {
    await build();
  } finally {
    PublicKey.findProgramAddressSync = original;
  }
  return programs;
}

test("builders and readers derive every Dusk PDA under their program's ID", async () => {
  assert.ok(!programId.equals(DUSK_PROGRAM_ID));
  const program = customProgram();
  const write = new DuskWrite(program);
  const tranches = new DuskTranches(program);
  const built = {};
  const programs = await derivationPrograms(async () => {
    built.initializeMarket = await write.initializeMarketInstruction({
      payer: owner, baseMint, quoteMint, srlpMint, baseHlpMint, quoteHlpMint,
      teamTreasury: key(30), teamTreasuryWsolAccount: key(31),
      config: defaultMarketLaunchConfig(),
      lpMetadata: Object.fromEntries(["srlp", "baseHlp", "quoteHlp"].map((kind) =>
        [kind, { name: "Omnipair LP", symbol: "LP", uri: "https://omnipair.fi/lp.json" }])),
    });
    built.borrow = await write.buildBorrowInstruction({
      market, owner, positionId, debtAssetMint: quoteMint, collateralAssetMint: baseMint,
      borrowAmount: 10n, minDebtAmountOut: 10n,
    });
    built.openLeverage = await write.buildOpenLeverageInstruction({
      market, owner, positionId, debtAsset: "quote", debtMint: quoteMint, collateralMint: baseMint,
      marginAmount: 10n, multiplierBps: 20_000n, minCollateralOut: 1n,
    });
    built.delegation = await write.buildCreateLeverageDelegationInstruction({
      market, owner, positionId, debtAsset: "quote", delegatedProgram: key(32), approvedActions: 1,
    });
    built.deposit = await write.depositCollateralInstruction({
      market, owner, positionId, assetMint: baseMint, ownerAssetAccount: key(33), depositAmount: 10n,
    });
    built.repay = await write.repayInstruction({
      market, owner, positionId, debtAssetMint: quoteMint, collateralAssetMint: baseMint,
      ownerDebtAccount: key(34), repayAmount: 10n, referralPartner: key(35),
    });
    built.addLiquidity = await write.addLiquidityInstruction({
      market, owner, baseMint, quoteMint, srlpMint, ownerBaseAccount: key(36), ownerQuoteAccount: key(37),
      ownerSrlpAccount: key(38), baseDepositAmount: 10n, quoteDepositAmount: 10n, minSrlpAmount: 1n,
    });
    built.swap = await write.buildSwapInstruction({
      market, trader: owner, assetInMint: baseMint, assetOutMint: quoteMint, exactAssetIn: 10n, minAssetOut: 1n,
    });
    built.closeLeverage = await write.closeLeverageInstruction({
      market, positionOwner: owner, positionId, debtMint: quoteMint, collateralMint: baseMint, debtAsset: "quote",
      minAmountOut: 1n, ownerDebtAccount: key(34), referralPartner: key(35),
    });
    built.withdrawAll = await write.withdrawAllCollateralInstruction({
      market, owner, positionId, baseMint, quoteMint, ownerBaseAccount: key(36), ownerQuoteAccount: key(37),
      minBaseOut: 0n, minQuoteOut: 0n,
    });
    built.withdrawRepaid = await write.withdrawRepaidLeverageInstruction({
      market, owner, positionId, collateralMint: baseMint, ownerCollateralAccount: key(36), minCollateralOut: 0n,
    });
    built.retiredHlpYield = await write.initializeYieldAccountsInstruction({
      payer: owner, owner, market, lpMint: oldHlpMint, baseMint, quoteMint, tokenKind: "hlp", retired: true,
    });
    built.retiredJuniorYield = await write.initializeYieldAccountsInstruction({
      payer: owner, owner, market, lpMint: key(39), baseMint, quoteMint, tokenKind: "junior", retired: true,
    });
    built.hook = await write.initializeLpTransferHookInstruction({ payer: owner, market, lpMint: baseHlpMint, tokenKind: "hlp" });
    built.rotate = await write.rotateHlpCohortInstruction({
      payer: owner, market, oldHlpMint, newHlpMint, targetAsset: "base",
      metadata: { name: "Omnipair hLP", symbol: "hLP", uri: "https://omnipair.fi/hlp.json" },
    });
    built.burnRetired = await write.burnRetiredHlpReceiptInstruction({
      market, owner, retiredMint: oldHlpMint, baseMint, quoteMint, amount: 1n,
    });
    built.configureReferral = await write.configureReferralPartnerInstruction({
      authoritySigner: owner, referrer, interestShareBps: 1_000, active: true,
    });
    built.referralAccrual = await write.initializeReferralAccrualInstruction({ payer: owner, referrer, market, assetMint: quoteMint });
    built.referralRecipient = await write.setReferralRecipientInstruction({ authority: referrer, recipient: owner });
    built.claimReferral = await write.claimReferralInterestInstruction({
      authority: referrer, market, mint: quoteMint, recipientTokenAccount: key(40),
    });
    built.referredBorrow = await write.referredBorrow(
      { borrowAmount: 10n, minDebtAmountOut: 10n },
      {
        payer: owner, referrer, market, debtMint: quoteMint,
        accounts: {
          futarchyAuthority: deriveFutarchyAuthorityAddress(programId)[0], owner, debtAssetMint: quoteMint,
          collateralAssetMint: baseMint, reserveVault: deriveMarketReserveVaultAddress(market, quoteMint, programId)[0],
          ownerDebtAccount: key(34), borrowPosition: deriveBorrowPositionAddress(market, owner, positionId, programId)[0],
          tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_PROGRAM_ID,
        },
      },
    );
    built.rollIncome = await tranches.rollIncome({ market, payer: owner, bookIndex: 1 });
    built.junior = await tranches.initialize({
      market, payer: owner, metadata: { name: "Omnipair jrLP", symbol: "jrLP", uri: "https://omnipair.fi/jr.json" },
    });
  });

  assert.ok(programs.includes(programId.toBase58()), "the custom deployment's PDAs were derived");
  assert.ok(!programs.includes(DUSK_PROGRAM_ID.toBase58()),
    "no Dusk PDA was derived under the default program while building for another deployment");

  const futarchy = at(deriveFutarchyAuthorityAddress(programId));
  const eventAuthority = at(deriveEventAuthorityAddress(programId));
  const paramsHash = await write.marketParamsHashFor({ payer: owner, config: defaultMarketLaunchConfig() });
  const marketAddress = at(deriveMarketAddress(baseMint, quoteMint, paramsHash, programId));
  const init = accountsOf(program, built.initializeMarket);
  assert.equal(init.market, marketAddress);
  assert.equal(init.futarchyAuthority, futarchy);
  assert.equal(init.eventAuthority, eventAuthority);
  assert.equal(init.program, programId.toBase58());
  const created = new PublicKey(marketAddress);
  for (const [side, mint] of [["base", baseMint], ["quote", quoteMint]]) {
    assert.equal(init[`${side}ReserveVault`], at(deriveMarketReserveVaultAddress(created, mint, programId)));
    assert.equal(init[`${side}CollateralVault`], at(deriveMarketCollateralVaultAddress(created, mint, programId)));
    assert.equal(init[`${side}InterestVault`], at(deriveMarketInterestVaultAddress(created, mint, programId)));
  }

  const borrow = accountsOf(program, built.borrow);
  assert.equal(borrow.futarchyAuthority, futarchy);
  assert.equal(borrow.reserveVault, at(deriveMarketReserveVaultAddress(market, quoteMint, programId)));
  assert.equal(borrow.borrowPosition, at(deriveBorrowPositionAddress(market, owner, positionId, programId)));
  assert.equal(borrow.eventAuthority, eventAuthority);

  const leveragePosition = deriveLeveragePositionAddress(market, owner, positionId, owner, programId)[0];
  const open = accountsOf(program, built.openLeverage);
  assert.equal(open.leveragePosition, leveragePosition.toBase58());
  assert.equal(open.leverageCollateralVault, at(deriveLeverageCollateralVaultAddress(market, baseMint, programId)));
  assert.equal(open.debtReserveVault, at(deriveMarketReserveVaultAddress(market, quoteMint, programId)));
  assert.equal(accountsOf(program, built.delegation).leverageDelegation,
    at(deriveLeverageDelegationAddress(leveragePosition, programId)));
  const close = accountsOf(program, built.closeLeverage);
  assert.equal(close.leveragePosition, leveragePosition.toBase58());
  assert.equal(close.debtInterestVault, at(deriveMarketInterestVaultAddress(market, quoteMint, programId)));
  assert.equal(close.referralAccrual, at(deriveReferralAccrualAddress(key(35), market, quoteMint, programId)));

  assert.equal(accountsOf(program, built.deposit).collateralVault,
    at(deriveMarketCollateralVaultAddress(market, baseMint, programId)));
  const repay = accountsOf(program, built.repay);
  assert.equal(repay.interestVault, at(deriveMarketInterestVaultAddress(market, quoteMint, programId)));
  assert.equal(repay.referralAccrual, at(deriveReferralAccrualAddress(key(35), market, quoteMint, programId)));
  assert.equal(accountsOf(program, built.withdrawAll).quoteCollateralVault,
    at(deriveMarketCollateralVaultAddress(market, quoteMint, programId)));
  assert.equal(accountsOf(program, built.withdrawRepaid).collateralVault,
    at(deriveLeverageCollateralVaultAddress(market, baseMint, programId)));
  const swap = accountsOf(program, built.swap);
  assert.equal(swap.futarchyAuthority, futarchy);
  assert.equal(swap.reserveOutVault, at(deriveMarketReserveVaultAddress(market, quoteMint, programId)));

  const liquidity = accountsOf(program, built.addLiquidity);
  assert.equal(liquidity.futarchyAuthority, futarchy);
  assert.equal(liquidity.baseYieldAccount, at(deriveYieldAccountAddress(market, owner, srlpMint, baseMint, "srlp", programId)));
  assert.equal(liquidity.quoteYieldAccount, at(deriveYieldAccountAddress(market, owner, srlpMint, quoteMint, "srlp", programId)));

  const hlpYield = accountsOf(program, built.retiredHlpYield);
  assert.equal(hlpYield.baseYieldAccount, at(deriveYieldAccountAddress(market, owner, oldHlpMint, baseMint, "hlp", programId)));
  assert.equal(hlpYield.futarchyAuthority, futarchy);
  assert.equal(built.retiredHlpYield.keys.at(-1).pubkey.toBase58(), at(deriveHlpCohortAddress(market, oldHlpMint, programId)));
  assert.equal(built.retiredJuniorYield.keys.at(-1).pubkey.toBase58(), at(deriveJuniorCohortAddress(market, key(39), programId)));
  assert.equal(accountsOf(program, built.hook).validationAccount, at(deriveYieldTransferHookValidationAddress(baseHlpMint, programId)));
  assert.equal(built.hook.keys.at(-1).pubkey.toBase58(), at(deriveHlpCohortAddress(market, baseHlpMint, programId)));
  assert.equal(accountsOf(program, built.rotate).oldCohort, at(deriveHlpCohortAddress(market, oldHlpMint, programId)));
  assert.equal(accountsOf(program, built.burnRetired).cohort, at(deriveHlpCohortAddress(market, oldHlpMint, programId)));

  const partner = at(deriveReferralPartnerAddress(referrer, programId));
  assert.equal(accountsOf(program, built.configureReferral).referralPartner, partner);
  assert.equal(accountsOf(program, built.configureReferral).futarchyAuthority, futarchy);
  const accrual = accountsOf(program, built.referralAccrual);
  assert.equal(accrual.referralPartner, partner);
  assert.equal(accrual.referralAccrual, at(deriveReferralAccrualAddress(new PublicKey(partner), market, quoteMint, programId)));
  assert.equal(accountsOf(program, built.referralRecipient).referralPartner, partner);
  const claim = accountsOf(program, built.claimReferral);
  assert.equal(claim.referralAccrual, accrual.referralAccrual);
  assert.equal(claim.interestVault, at(deriveMarketInterestVaultAddress(market, quoteMint, programId)));
  assert.equal(built.referredBorrow.referralPartner.toBase58(), partner);
  assert.equal(accountsOf(program, built.referredBorrow.actionInstruction).referralAccrual, accrual.referralAccrual);

  assert.equal(built.rollIncome.incomeEpoch.toBase58(), at(deriveIncomeEpochAddress(market, 1, 4n, programId)));
  assert.equal(accountsOf(program, built.rollIncome.instruction).incomeEpoch, built.rollIncome.incomeEpoch.toBase58());
  const juniorMint = deriveJuniorMintAddress(market, 0n, programId)[0];
  assert.ok(built.junior.juniorMint.equals(juniorMint));
  assert.equal(built.junior.cohort.toBase58(), at(deriveJuniorCohortAddress(market, juniorMint, programId)));
});

test("readers, delegate clients and the Dusk client follow the program ID", async () => {
  const program = customProgram();
  const get = new DuskGet(program);
  assert.equal(at(get.pda.futarchyAuthority()), at(deriveFutarchyAuthorityAddress(programId)));
  assert.equal(at(get.pda.marketReserveVault(market, baseMint)), at(deriveMarketReserveVaultAddress(market, baseMint, programId)));
  assert.equal(at(get.pda.leveragePosition(market, owner, positionId)),
    at(deriveLeveragePositionAddress(market, owner, positionId, owner, programId)));
  assert.equal(at(get.pda.referralPartner(referrer)), at(deriveReferralPartnerAddress(referrer, programId)));
  // The module-level helpers keep the canonical deployment, and an explicit ID still wins.
  assert.equal(at(pda.futarchyAuthority()), at(deriveFutarchyAuthorityAddress()));
  assert.equal(at(get.pda.futarchyAuthority(DUSK_PROGRAM_ID)), at(deriveFutarchyAuthorityAddress()));
  let fetched;
  program.account.futarchyAuthority.fetch = async (account) => { fetched = account; return {}; };
  await get.futarchyAuthority();
  assert.equal(fetched.toBase58(), at(deriveFutarchyAuthorityAddress(programId)));

  const delegate = createLeverageDelegateProgram({ provider });
  const programs = await derivationPrograms(async () => {
    const order = await new DuskLeverageOrders(delegate, programId).createOrderInstruction({
      market, owner, positionId, orderId: 1n, kind: "stopLoss", triggerPriceNad: 1n, closeBps: 10_000, minOwnerPayout: 1n,
    });
    assert.ok(order.keys[1].pubkey.equals(deriveLeveragePositionAddress(market, owner, positionId, owner, programId)[0]));
    const protection = await new DuskProtectionOrders(delegate, programId).createInstruction({
      orderId: 2n, action: PROTECTION_ACTION.borrowRepay, debtAsset: 1, lpAmount: 10n, maxLpPerExecution: 1n,
      maxPaymentPerExecution: 1n, triggerHealthBps: 11_000n, targetHealthBps: 12_000n, minPaymentPerLpNad: 1n,
      keeperFeeBps: 30, expiresAt: 1_900_000_000n,
    }, {
      market, borrowPosition: key(41), leveragePosition: null, lpMint: srlpMint, baseMint, quoteMint,
      ownerLpAccount: key(42), custodyLpAccount: key(43), baseYieldAccount: key(44), quoteYieldAccount: key(45), owner,
    });
    const spec = delegate.idl.instructions.find((candidate) => candidate.name === "createProtectionOrder").accounts;
    const keyOf = (name) => protection.keys[spec.findIndex((account) => account.name === name)].pubkey;
    assert.ok(keyOf("duskProgram").equals(programId));
    assert.ok(keyOf("duskEventAuthority").equals(deriveEventAuthorityAddress(programId)[0]));
  });
  assert.ok(!programs.includes(DUSK_PROGRAM_ID.toBase58()));

  const dusk = new Dusk({ provider, programId });
  assert.ok(dusk.program.programId.equals(programId));
  assert.ok(dusk.orders.duskProgramId.equals(programId));
  assert.equal(at(dusk.get.pda.marketInterestVault(market, quoteMint)), at(deriveMarketInterestVaultAddress(market, quoteMint, programId)));
});
