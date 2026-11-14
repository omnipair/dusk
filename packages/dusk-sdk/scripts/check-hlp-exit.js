import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair, TransactionInstruction } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DuskHlpLiquidations } from "../dist/hlp-liquidation-client.js";
import { MARKET_LAYOUT_VERSION } from "../dist/constants.js";

const U64_MAX = (1n << 64n) - 1n;

function fixture() {
  const keys = Array.from({ length: 13 }, (_, i) => Keypair.fromSeed(new Uint8Array(32).fill(i + 1)).publicKey);
  const [programId, market, owner, baseMint, quoteMint, baseReserve, quoteReserve,
    baseInterest, quoteInterest, baseHlpMint, quoteHlpMint, srlpMint, routeProgram] = keys;
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID }) },
    wallet: { publicKey: owner }, publicKey: owner,
  });
  // No hLP supply, so settlement takes no hLP prefix.
  const inactive = { hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0) };
  program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION,
    amm: { frozenHlpMask: 1 }, srlpMint,
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, interestVault: baseInterest, hlpMint: baseHlpMint },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, interestVault: quoteInterest, hlpMint: quoteHlpMint },
    baseHlpVault: { srlpVault: baseReserve, debtPrincipal: new anchor.BN(1_000), ...inactive },
    quoteHlpVault: { srlpVault: quoteReserve, debtPrincipal: new anchor.BN(5_000), ...inactive },
  });
  const client = new DuskHlpLiquidations(program);
  const setup = new TransactionInstruction({ programId: baseMint, keys: [], data: Buffer.alloc(0) });
  const exit = new TransactionInstruction({ programId: baseHlpMint, keys: [], data: Buffer.alloc(0) });
  const withdrawals = [];
  client.write.withdrawSingleSided = async (amounts, options) => {
    withdrawals.push(amounts);
    assert.ok(options.owner.equals(owner));
    return { setupInstructions: [setup], actionInstruction: exit };
  };
  const exitOptions = { market, owner, targetHlpMint: baseHlpMint, baseMint, quoteMint };
  const route = new TransactionInstruction({ programId: routeProgram, keys: [], data: Buffer.alloc(0) });
  return { program, client, market, owner, setup, exit, withdrawals, exitOptions, route };
}

const quote = (full, fields = {}) => ({ targetAsset: 0, full,
  payment: new anchor.BN(100), collateralDebit: new anchor.BN(120), collateralCredit: new anchor.BN(120),
  borrowedClaim: new anchor.BN(600), reserveCredit: new anchor.BN(98), discountBps: 0, ...fields });
const beginArgs = (program, plan) => program.coder.instruction.decode(plan.beginInstruction.data).data.args;

test("a holder can close a terminal hLP vault and burn receipts atomically", async () => {
  const { program, client, market, owner, setup, exit, withdrawals, exitOptions, route } = fixture();
  assert.equal(await client.isFrozen({ market, targetAsset: "base" }), true);
  assert.equal(await client.isFrozen({ market, targetAsset: "quote" }), false);
  // A terminal burn pays no principal, so an explicit zero floor is accepted.
  const holderExit = { hlpAmount: 25n, minTargetAmountOut: 0n, options: exitOptions };
  const native = await client.buildNativeCloseAndExit({ market, targetAsset: "base", exit: holderExit, maxSocializedLoss: 700n });
  const close = program.coder.instruction.decode(native.closeInstruction.data);
  assert.equal(close.name, "closeInsolventHlp");
  assert.equal(close.data.args.maxSocializedLoss.toString(), "700");
  assert.deepEqual(native.instructions.slice(1), [setup, native.closeInstruction, exit]);
  assert.deepEqual(withdrawals.at(-1), { hlpAmount: "25", minTargetAmountOut: "0" });

  client.preview = async () => quote(false);
  const flash = await client.buildFlash({ market, targetAsset: "base", buyer: owner, maxSocializedLoss: 0n,
    exit: { ...holderExit, minTargetAmountOut: 90n }, settleRemainingAccounts: [], route: () => [route] });
  assert.equal(flash.instructions[flash.beginIndex], flash.beginInstruction);
  assert.equal(flash.instructions[flash.settleIndex], flash.settleInstruction);
  assert.equal(flash.instructions[flash.beginIndex - 3], setup);
  assert.equal(flash.instructions.at(-1), exit);
  assert.equal(beginArgs(program, flash).settleIndex, flash.settleIndex);
  assert.deepEqual(withdrawals.at(-1), { hlpAmount: "25", minTargetAmountOut: "90" });
  await assert.rejects(client.buildNativeCloseAndExit({ market, targetAsset: "quote", exit: holderExit, maxSocializedLoss: 0n }),
    /Exit hLP mint differs/);
});

test("hLP recovery builders require a socialized-loss cap and an exit floor", async () => {
  const { client, market, owner, exitOptions, route } = fixture();
  let previews = 0;
  client.preview = async () => { previews++; return quote(false); };
  const holderExit = { hlpAmount: 25n, minTargetAmountOut: 0n, options: exitOptions };
  // No implicit u64::MAX loss tolerance on either path.
  await assert.rejects(client.buildNativeCloseAndExit({ market, targetAsset: "base", exit: holderExit }),
    /maxSocializedLoss is required/);
  await assert.rejects(client.buildFlash({ market, targetAsset: "base", buyer: owner, route: () => [route] }),
    /maxSocializedLoss is required/);
  assert.equal(previews, 0);
  // No implicit zero payout floor: omitted everywhere, and zero after a solvent rescue.
  const { minTargetAmountOut, ...unbounded } = holderExit;
  await assert.rejects(client.buildNativeCloseAndExit({ market, targetAsset: "base", exit: unbounded, maxSocializedLoss: 0n }),
    /minTargetAmountOut is required/);
  await assert.rejects(client.buildFlash({ market, targetAsset: "base", buyer: owner, maxSocializedLoss: 0n,
    exit: unbounded, route: () => [route] }), /minTargetAmountOut is required/);
  await assert.rejects(client.buildFlash({ market, targetAsset: "base", buyer: owner, maxSocializedLoss: 0n,
    exit: holderExit, route: () => [route] }), /positive exit\.minTargetAmountOut/);
});

test("a reviewed hLP flash quote bounds the plan and its principal write-off", async () => {
  const { program, client, market, owner, withdrawals, exitOptions, route } = fixture();
  client.preview = async () => { throw new Error("a supplied quote replaces the builder's preview"); };
  let context;
  const full = quote(true, { payment: new anchor.BN(150), collateralDebit: new anchor.BN(400), collateralCredit: new anchor.BN(390) });
  const terminal = await client.buildFlash({ market, targetAsset: "base", buyer: owner, allowFull: true, quote: full,
    exit: { hlpAmount: 25n, minTargetAmountOut: 0n, options: exitOptions },
    route: (ctx) => { context = ctx; return [route]; } });
  const args = beginArgs(program, terminal);
  // Base-target vault principal 1,000 less recovery 600 + 98.
  assert.equal(args.maxSocializedLoss.toString(), "302");
  assert.equal(args.maxPayment.toString(), "150");
  assert.equal(args.maxCollateralDebit.toString(), "400");
  assert.equal(args.minCollateralCredit.toString(), "390");
  assert.equal(args.allowFull, true);
  assert.equal(context.quote, full);
  assert.equal(context.requiredPaymentCredit, 150n);
  assert.deepEqual(withdrawals.at(-1), { hlpAmount: "25", minTargetAmountOut: "0" });

  const covered = await client.buildFlash({ market, targetAsset: "base", buyer: owner, allowFull: true,
    quote: quote(true, { borrowedClaim: new anchor.BN(2_000) }), route: () => [route] });
  assert.equal(beginArgs(program, covered).maxSocializedLoss.toString(), "0");
  const partial = await client.buildFlash({ market, targetAsset: "base", buyer: owner, quote: quote(false), route: () => [route] });
  assert.equal(beginArgs(program, partial).maxSocializedLoss.toString(), "0");
  // An explicit cap overrides the derived one.
  const explicit = await client.buildFlash({ market, targetAsset: "base", buyer: owner, allowFull: true, quote: full,
    maxSocializedLoss: U64_MAX, route: () => [route] });
  assert.equal(beginArgs(program, explicit).maxSocializedLoss.toString(), U64_MAX.toString());

  await assert.rejects(client.buildFlash({ market, targetAsset: "base", buyer: owner, quote: full, route: () => [route] }),
    /set allowFull explicitly/);
  await assert.rejects(client.buildFlash({ market, targetAsset: "quote", buyer: owner, quote: full, route: () => [route] }),
    /other hLP vault/);
});
