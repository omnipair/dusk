import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Keypair } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";

for (const debtAsset of ["base", "quote"]) for (const tokenProgram of [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID]) {
  test(`leverage builder funds and pays out ${debtAsset} debt with ${tokenProgram}`, async () => {
    const [market, owner, positionId, debtMint, collateralMint] = Array.from({ length: 5 }, () => Keypair.generate().publicKey);
    const connection = { getAccountInfo: async (key) => {
      assert.ok(key.equals(debtMint), "only the required funding mint determines its ATA program");
      return { owner: tokenProgram };
    } };
    const program = new anchor.Program(IDL, { connection, publicKey: owner, wallet: { publicKey: owner } });
    program.account.market.fetch = async () => ({
      baseHlpVault: { hlpSupply: 0n, residualExposure: 0n },
      quoteHlpVault: { hlpSupply: 0n, residualExposure: 0n },
    });
    const write = new DuskWrite(program);
    const request = { market, owner, positionId, debtAsset, debtMint, collateralMint,
      marginAmount: 1000n, multiplierBps: 30_000n, minCollateralOut: 1n };
    const opened = await write.buildOpenLeverageInstruction(request);
    const decoded = program.coder.instruction.decode(opened.data);
    assert.equal(decoded.name, "openLeverage");
    assert.equal(decoded.data.args.marginAmount.toString(), "1000");
    assert.equal(decoded.data.args.debtAsset, debtAsset === "base" ? 0 : 1);
    const ownerDebtAccount = getAssociatedTokenAddressSync(debtMint, owner, true, tokenProgram);
    const accountIndex = (ix, name) => program.idl.instructions.find(i => i.name === ix).accounts.findIndex(a => a.name === name);
    assert.ok(opened.keys[accountIndex("openLeverage", "ownerDebtAccount")].pubkey.equals(ownerDebtAccount));
    const close = { ...request, positionOwner: owner, ownerDebtAccount, minAmountOut: 1n };
    const closed = await write.closeLeverageInstruction(close);
    assert.equal(program.coder.instruction.decode(closed.data).name, "closeLeverage");
    assert.ok(closed.keys[accountIndex("closeLeverage", "ownerDebtAccount")].pubkey.equals(ownerDebtAccount));
    // Old JavaScript callers must fail rather than silently select another funding/payout asset.
    await assert.rejects(write.buildOpenLeverageInstruction({ ...request, fundingAsset: "collateral" }), /debt asset/);
    await assert.rejects(write.closeLeverageInstruction({ ...close, collateralFunded: true }), /no longer supported/);
  });
}
