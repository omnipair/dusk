import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair, TransactionInstruction } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DuskHlpLiquidations } from "../dist/hlp-liquidation-client.js";

test("a holder can close a terminal hLP vault and burn receipts atomically", async () => {
  const keys = Array.from({ length: 13 }, (_, i) => Keypair.fromSeed(new Uint8Array(32).fill(i + 1)).publicKey);
  const [programId, market, owner, baseMint, quoteMint, baseReserve, quoteReserve,
    baseInterest, quoteInterest, baseHlpMint, quoteHlpMint, ylpMint, insurance] = keys;
  const program = new anchor.Program({ ...IDL, address: programId.toBase58() }, {
    connection: { getAccountInfo: async () => ({ owner: TOKEN_PROGRAM_ID }) },
    wallet: { publicKey: owner }, publicKey: owner,
  });
  program.account.market.fetch = async () => ({
    amm: { frozenHlpMask: 1 }, ylpMint,
    baseSide: { assetMint: baseMint, reserveVault: baseReserve, interestVault: baseInterest, hlpMint: baseHlpMint },
    quoteSide: { assetMint: quoteMint, reserveVault: quoteReserve, interestVault: quoteInterest, hlpMint: quoteHlpMint },
    baseHlpVault: { ylpVault: baseReserve }, quoteHlpVault: { ylpVault: quoteReserve },
    insurance: { baseVault: insurance, quoteVault: insurance },
  });
  const client = new DuskHlpLiquidations(program);
  assert.equal(await client.isFrozen({ market, targetAsset: "base" }), true);
  assert.equal(await client.isFrozen({ market, targetAsset: "quote" }), false);
  const setup = new TransactionInstruction({ programId: baseMint, keys: [], data: Buffer.alloc(0) });
  const exit = new TransactionInstruction({ programId: baseHlpMint, keys: [], data: Buffer.alloc(0) });
  client.write.withdrawSingleSided = async (amounts, options) => {
    assert.equal(amounts.hlpAmount.toString(), "25");
    assert.equal(amounts.minTargetAmountOut.toString(), "0");
    assert.ok(options.owner.equals(owner));
    return { setupInstructions: [setup], actionInstruction: exit };
  };
  const holderExit = { hlpAmount: 25n, options: {
    market, owner, targetHlpMint: baseHlpMint, baseMint, quoteMint,
  } };
  const native = await client.buildNativeCloseAndExit({ market, targetAsset: "base", exit: holderExit });
  assert.equal(program.coder.instruction.decode(native.closeInstruction.data).name, "closeInsolventHlp");
  assert.deepEqual(native.instructions.slice(1), [setup, native.closeInstruction, exit]);

  client.preview = async () => ({ full: false, payment: new anchor.BN(100),
    collateralDebit: new anchor.BN(120), collateralCredit: new anchor.BN(120) });
  const route = new TransactionInstruction({ programId: insurance, keys: [], data: Buffer.alloc(0) });
  const flash = await client.buildFlash({ market, targetAsset: "base", buyer: owner,
    exit: holderExit, settleRemainingAccounts: [], route: () => [route] });
  assert.equal(flash.instructions[flash.beginIndex], flash.beginInstruction);
  assert.equal(flash.instructions[flash.settleIndex], flash.settleInstruction);
  assert.equal(flash.instructions[flash.beginIndex - 3], setup);
  assert.equal(flash.instructions.at(-1), exit);
  assert.equal(program.coder.instruction.decode(flash.beginInstruction.data).data.args.settleIndex, flash.settleIndex);
  await assert.rejects(client.buildNativeCloseAndExit({ market, targetAsset: "quote", exit: holderExit }),
    /Exit hLP mint differs/);
});
