import assert from "node:assert/strict";
import { test } from "node:test";
import anchor from "@coral-xyz/anchor";
import {
  getAssociatedTokenAddressSync,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";

import IDL from "../dist/idl_v2.js";
import { DUSK_PROGRAM_ID, SEEDS, deriveInsuranceAddress } from "../dist/constants.js";
import { DuskWrite } from "../dist/write.js";

const keys = Array.from({ length: 5 }, (_, i) =>
  Keypair.fromSeed(new Uint8Array(32).fill(i + 1)).publicKey
);
const [donor, market, assetMint, customProgram, customAccount] = keys;

function fixture(getAccountInfo = async () => { throw new Error("unexpected RPC read"); }) {
  const program = new anchor.Program({ ...IDL, address: customProgram.toBase58() }, {
    connection: { getAccountInfo },
    wallet: { publicKey: donor },
    publicKey: donor,
  });
  program.account.market.fetch = async () => { throw new Error("market does not exist yet"); };
  return { program, write: new DuskWrite(program) };
}

const base = { donor, market, asset: "base", assetMint, amount: 1_000_000n };

for (const [asset, assetTokenProgram, amount] of [
  ["base", TOKEN_PROGRAM_ID, 1_000_000n],
  ["quote", TOKEN_2022_PROGRAM_ID, (1n << 64n) - 1n],
]) {
  test(`insurance donation encodes ${asset} and its exact u64 amount without fetching a market`, async () => {
    const { program, write } = fixture();
    const ix = await write.fortifyMarketInstruction({ ...base, asset, assetTokenProgram, amount });
    const decoded = program.coder.instruction.decode(ix.data);
    assert.equal(decoded.name, "fortifyMarket");
    assert.equal(decoded.data.args.asset, asset === "base" ? 0 : 1);
    assert.equal(decoded.data.args.amount.toString(), amount.toString());
    assert.ok(ix.programId.equals(customProgram));
    const names = program.idl.instructions.find(x => x.name === "fortifyMarket").accounts;
    const meta = name => ix.keys[names.findIndex(x => x.name === name)];
    assert.ok(meta("donor").pubkey.equals(donor));
    assert.ok(meta("donor").isSigner);
    assert.ok(meta("donorAssetAccount").pubkey.equals(
      getAssociatedTokenAddressSync(assetMint, donor, true, assetTokenProgram)
    ));
    const [expectedVault] = PublicKey.findProgramAddressSync(
      [SEEDS.INSURANCE, market.toBuffer(), assetMint.toBuffer()], customProgram
    );
    assert.ok(meta("insuranceVault").pubkey.equals(expectedVault));
    assert.ok(meta("insuranceVault").isWritable);
    assert.ok(meta("tokenProgram").pubkey.equals(TOKEN_PROGRAM_ID));
    assert.ok(meta("token2022Program").pubkey.equals(TOKEN_2022_PROGRAM_ID));
    assert.ok(meta("program").pubkey.equals(customProgram));
    const [eventAuthority] = PublicKey.findProgramAddressSync([SEEDS.EVENT_AUTHORITY], customProgram);
    assert.ok(meta("eventAuthority").pubkey.equals(eventAuthority));
  });
}

test("automatic mint-owner lookup chooses the Token-2022 ATA with one RPC read", async () => {
  let reads = 0;
  const { write, program } = fixture(async mint => {
    assert.ok(mint.equals(assetMint));
    reads += 1;
    return { owner: TOKEN_2022_PROGRAM_ID };
  });
  const ix = await write.fortifyMarketInstruction(base);
  assert.equal(reads, 1);
  const instruction = program.idl.instructions.find(x => x.name === "fortifyMarket");
  const source = instruction.accounts.findIndex(x => x.name === "donorAssetAccount");
  assert.ok(ix.keys[source].pubkey.equals(
    getAssociatedTokenAddressSync(assetMint, donor, true, TOKEN_2022_PROGRAM_ID)
  ));
});

test("custom source accounts are preserved and the transaction builder does not send", async () => {
  const { write, program } = fixture();
  const transaction = await write.fortifyMarketTransaction({
    ...base, donorAssetAccount: customAccount, assetTokenProgram: TOKEN_PROGRAM_ID,
  });
  assert.equal(transaction.instructions.length, 1);
  assert.equal(transaction.signatures.length, 0);
  const instruction = program.idl.instructions.find(x => x.name === "fortifyMarket");
  const source = instruction.accounts.findIndex(x => x.name === "donorAssetAccount");
  assert.ok(transaction.instructions[0].keys[source].pubkey.equals(customAccount));
});

test("invalid amounts, sides and explicit token programs fail before instruction construction", async () => {
  const { write } = fixture();
  const params = { ...base, assetTokenProgram: TOKEN_PROGRAM_ID };
  for (const amount of [0n, -1n, 1n << 64n, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    await assert.rejects(write.fortifyMarketInstruction({ ...params, amount }));
  }
  await assert.rejects(write.fortifyMarketInstruction({ ...params, asset: "other" }), /asset/);
  await assert.rejects(write.fortifyMarketInstruction({
    ...params, assetTokenProgram: SystemProgram.programId,
  }), /SPL Token or Token-2022/);
});

test("missing or unsupported mint owners cannot silently use a legacy ATA", async () => {
  const missing = fixture(async () => null);
  await assert.rejects(missing.write.fortifyMarketInstruction(base), /Mint account not found/);
  const unsupported = fixture(async () => ({ owner: SystemProgram.programId }));
  await assert.rejects(unsupported.write.fortifyMarketInstruction(base), /Unsupported mint owner/);
});

test("insurance PDA default remains compatible with existing callers", () => {
  assert.ok(deriveInsuranceAddress(market, assetMint)[0].equals(
    deriveInsuranceAddress(market, assetMint, DUSK_PROGRAM_ID)[0]
  ));
});
