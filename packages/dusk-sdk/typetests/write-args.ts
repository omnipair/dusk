/**
 * Compile-time checks for the generic write path and the composite builders.
 *
 * `npm run typecheck` covers this file and never runs it. Each
 * `@ts-expect-error` line must fail to compile: if a change to the instruction
 * types lets one through, the directive is unused and the typecheck fails.
 * Each negative case stays on one line because the directive only covers the
 * line after it.
 */
import { BN, type Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import { DuskWrite, type DuskBuildOptions, type DuskIdl, type HlpLiquidityBuildOptions } from "../src/index.js";

declare const program: Program<DuskIdl>;
const write = new DuskWrite(program);
const key = PublicKey.default;

// `market` and `borrowPosition` are PDAs, but their seeds read their own data
// (`market.params_hash`, `borrow_position.owner`), so Anchor cannot derive
// them.
const borrowAccounts = {
  market: key,
  debtAssetMint: key,
  collateralAssetMint: key,
  reserveVault: key,
  ownerDebtAccount: key,
  borrowPosition: key,
  referralPartner: null,
  referralAccrual: null,
};
const { market: borrowMarket, ...borrowWithoutMarket } = borrowAccounts;
const { borrowPosition, ...borrowWithoutPosition } = borrowAccounts;
void borrowMarket;
void borrowPosition;
const borrowArgs = { borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBps: 0 };
const yieldAccounts = { market: key, assetMint: key, lpMint: key, yieldAccount: key };
// A nested group whose `market` reads its own data like a top-level one.
const flashLiquidationAccounts = {
  market: key,
  borrowPosition: key,
  leveragePosition: null,
  collateralMint: key,
  debtMint: key,
  collateralVault: key,
  reserveVault: key,
  interestVault: key,
  buyerCollateralAccount: key,
  buyerRefundAccount: key,
  ownerDebtAccount: key,
  referralPartner: null,
  referralAccrual: null,
  debtTokenProgram: key,
};
const { market: flashMarket, ...flashWithoutMarket } = flashLiquidationAccounts;
void flashMarket;
const flashLiquidationArgs = {
  position: key,
  debtAsset: 0,
  maxRepayment: 1n,
  maxPayment: 1n,
  minCollateralCredit: 1n,
  minBuyerCredit: 0n,
  full: true,
  settleIndex: 0,
};

export async function genericPath(): Promise<void> {
  // Integers in any form, as the argument itself or a one-element tuple.
  await write.instruction("borrow", { borrowAmount: 1n, minDebtAmountOut: "2", minLiquidationCfBps: 0, referrer: null }, { accounts: borrowAccounts });
  await write.transaction("borrow", [{ borrowAmount: new BN(1), minDebtAmountOut: 2, minLiquidationCfBps: 0 }], { accounts: borrowAccounts });
  write.builder("borrow", borrowArgs, { accounts: borrowAccounts, remainingAccounts: [] });
  write.method("borrow", borrowArgs);

  // @ts-expect-error a misspelled field
  await write.instruction("borrow", { borrowAmount: 1n, minDebtAmountOt: 2n, minLiquidationCfBps: 0 }, { accounts: borrowAccounts });
  // @ts-expect-error a missing field
  await write.instruction("borrow", { borrowAmount: 1n, minLiquidationCfBps: 0 }, { accounts: borrowAccounts });
  // @ts-expect-error a wrongly typed field
  await write.rpc("borrow", { borrowAmount: true, minDebtAmountOut: 2n, minLiquidationCfBps: 0 }, { accounts: borrowAccounts });
  // @ts-expect-error a u16 is a number
  await write.instruction("borrow", { borrowAmount: 1n, minDebtAmountOut: 2n, minLiquidationCfBps: 0n }, { accounts: borrowAccounts });
  // @ts-expect-error arguments are required
  await write.instruction("borrow");
  // @ts-expect-error arguments are required by `method` too
  write.method("borrow");
  // @ts-expect-error a misspelled account
  await write.instruction("borrow", borrowArgs, { accounts: { ...borrowAccounts, ownerDebtAcount: key } });
  // @ts-expect-error an account Anchor cannot resolve is required
  await write.instruction("borrow", borrowArgs, { accounts: { market: key, debtAssetMint: key, collateralAssetMint: key, reserveVault: key, borrowPosition: key, referralPartner: null, referralAccrual: null } });
  // @ts-expect-error so the accounts are too
  await write.transaction("borrow", borrowArgs);
  // @ts-expect-error a PDA whose seeds read its own data is required: the market
  await write.instruction("borrow", borrowArgs, { accounts: borrowWithoutMarket });
  // @ts-expect-error and the borrow position
  await write.instruction("borrow", borrowArgs, { accounts: borrowWithoutPosition });
  // @ts-expect-error even when it lists a relation, which Anchor does not try for a PDA it cannot derive
  await write.instruction("executeOperatorChange", undefined, { accounts: { proposal: key, previousRevenue: null } });
  await write.instruction("executeOperatorChange", undefined, { accounts: { market: key, proposal: key, previousRevenue: null } });
  // A relation alone still resolves: `rentRefund` comes from the proposal.
  await write.instruction("resolveOperatorChange", false, { accounts: { market: key, proposal: key } });
  // A self-seeded PDA inside a nested group is required too.
  await write.instruction("beginFlashLiquidation", flashLiquidationArgs, { accounts: { accounts: flashLiquidationAccounts, repaymentVault: key } });
  // @ts-expect-error the nested market
  await write.instruction("beginFlashLiquidation", flashLiquidationArgs, { accounts: { accounts: flashWithoutMarket, repaymentVault: key } });
  // @ts-expect-error null means "none" only for an optional account
  await write.instruction("borrow", borrowArgs, { accounts: { ...borrowAccounts, reserveVault: null } });
  // @ts-expect-error not an instruction
  await write.instruction("borow", borrowArgs, { accounts: borrowAccounts });

  // An instruction without arguments takes none.
  await write.instruction("previewMarket", undefined, { accounts: { market: key } });
  await write.instruction("previewMarket", [], { accounts: { market: key } });
  // @ts-expect-error it takes no arguments
  await write.instruction("previewMarket", { market: key }, { accounts: { market: key } });

  // An enum takes exactly one known variant.
  await write.instruction("setYieldRecipient", { tokenKind: { hlp: {} }, recipient: key }, { accounts: yieldAccounts });
  // @ts-expect-error two variants
  await write.instruction("setYieldRecipient", { tokenKind: { hlp: {}, srlp: {} }, recipient: key }, { accounts: yieldAccounts });
  // @ts-expect-error an unknown variant
  await write.instruction("setYieldRecipient", { tokenKind: { Hlp: {} }, recipient: key }, { accounts: yieldAccounts });
  // An Option may be left out.
  await write.instruction("setHarvestAuthority", { tokenKind: { srlp: {} } }, { accounts: yieldAccounts });

  // A name chosen at run time takes either candidate's arguments.
  const deposit = Date.now() % 2 === 0;
  await write.instruction(
    deposit ? "previewAddLiquidity" : "previewMarket",
    deposit ? { baseDepositAmount: 1n, quoteDepositAmount: 1n } : undefined,
    { accounts: { market: key, ...(deposit ? { baseMint: key, quoteMint: key } : {}) } }
  );

  // Options typed for one instruction do not fit another.
  const swapOptions: DuskBuildOptions<"swap"> = {};
  // @ts-expect-error swap options are not borrow options
  const borrowOptions: DuskBuildOptions<"borrow"> = swapOptions;
  void borrowOptions;
}

export async function composites(): Promise<void> {
  // The swap composite fills in the account map's market from `market`.
  const swapAccounts = {
    assetInMint: key,
    assetOutMint: key,
    reserveInVault: key,
    reserveOutVault: key,
    traderAssetInAccount: key,
    traderAssetOutAccount: key,
  };
  await write.swapInstruction({ exactAssetIn: 1n, minAssetOut: "2" }, { market: key, accounts: swapAccounts });
  // A caller may still name it; the composite's value wins.
  await write.swapBuilder({ exactAssetIn: 1n, minAssetOut: 2n }, { market: key, accounts: { ...swapAccounts, market: key } });
  // @ts-expect-error a misspelled amount
  await write.swapTransaction({ exactAssetIn: 1n, minAssetOt: 2n }, { market: key, accounts: swapAccounts });
  // @ts-expect-error a swap account is missing
  await write.swapRpc({ exactAssetIn: 1n, minAssetOut: 2n }, { market: key, accounts: { assetInMint: key, assetOutMint: key } });
  // @ts-expect-error the market option is required
  await write.swapInstruction({ exactAssetIn: 1n, minAssetOut: 2n }, { accounts: { ...swapAccounts, market: key } });

  const depositAccounts = {
    futarchyAuthority: key,
    srlpMint: key,
    baseReserveVault: key,
    quoteReserveVault: key,
    ownerTargetAccount: key,
    ownerHlpAccount: key,
    hlpSrlpAccount: key,
  };
  const hlp = { payer: key, owner: key, market: key, targetHlpMint: key, baseMint: key, quoteMint: key };
  const withdrawOptions: HlpLiquidityBuildOptions<"withdrawSingleSided"> = {
    ...hlp,
    accounts: { ...depositAccounts, borrowedInterestVault: key },
  };
  await write.depositSingleSided({ depositAmount: 1n, minHlpAmount: 0 }, { ...hlp, accounts: depositAccounts });
  await write.withdrawSingleSided({ hlpAmount: "1", minTargetAmountOut: 0n }, withdrawOptions);
  // @ts-expect-error composite amounts are bigint, number or string
  await write.depositSingleSided({ depositAmount: new BN(1), minHlpAmount: 0n }, { ...hlp, accounts: depositAccounts });
  // @ts-expect-error a missing slippage floor
  await write.withdrawSingleSided({ hlpAmount: 1n }, withdrawOptions);
  // @ts-expect-error a withdrawal needs the borrowed side's interest vault
  await write.withdrawSingleSided({ hlpAmount: 1n, minTargetAmountOut: 0n }, { ...hlp, accounts: depositAccounts });
  await write.hlpLiquidityAction(
    Date.now() % 2 === 0 ? "depositSingleSided" : "withdrawSingleSided",
    Date.now() % 2 === 0 ? { depositAmount: 1n, minHlpAmount: 0n } : { hlpAmount: 1n, minTargetAmountOut: 0n },
    withdrawOptions
  );

  const referral = { payer: key, referrer: key, market: key, debtMint: key };
  await write.referredBorrow({ borrowAmount: 1n, minDebtAmountOut: 1n }, { ...referral, accounts: borrowAccounts });
  // @ts-expect-error a misspelled optional field
  await write.referredBorrow({ borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBp: 1 }, { ...referral, accounts: borrowAccounts });
  // The referred composites fill in the market and the referral accounts.
  await write.referredBorrow({ borrowAmount: 1n, minDebtAmountOut: 1n }, { ...referral, accounts: borrowWithoutMarket });
  // @ts-expect-error but not the borrow position, which has no id among the borrow parameters
  await write.referredBorrow({ borrowAmount: 1n, minDebtAmountOut: 1n }, { ...referral, accounts: borrowWithoutPosition });
  const leverageAccounts = {
    futarchyAuthority: key,
    leveragePosition: key,
    debtMint: key,
    collateralMint: key,
    debtReserveVault: key,
    collateralReserveVault: key,
    leverageCollateralVault: key,
    ownerDebtAccount: key,
  };
  const leverage = { positionId: key, marginAmount: 1n, multiplierBps: 20_000, minCollateralOut: 1n };
  await write.referredOpenLeverage({ ...leverage, debtAsset: "quote" }, { ...referral, accounts: leverageAccounts });
  await write.referredOpenLeverage({ ...leverage, debtAsset: 1, limitPriceNad: 0n, positionOwner: null }, { ...referral, accounts: leverageAccounts });
  // @ts-expect-error the debt side is "base", "quote", 0 or 1
  await write.referredOpenLeverage({ ...leverage, debtAsset: 2 }, { ...referral, accounts: leverageAccounts });

  // The typed builders keep their signatures.
  await write.buildBorrowInstruction({ market: key, owner: key, positionId: key, debtAssetMint: key, collateralAssetMint: key, borrowAmount: 1n, minDebtAmountOut: 1n });
  await write.addLiquidityTransaction({ market: key, owner: key, baseMint: key, quoteMint: key, srlpMint: key, ownerBaseAccount: key, ownerQuoteAccount: key, ownerSrlpAccount: key, baseDepositAmount: 1n, quoteDepositAmount: 1n, minSrlpAmount: 0n });
}
