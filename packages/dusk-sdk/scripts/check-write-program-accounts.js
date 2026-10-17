import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey } from "@solana/web3.js";

import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";

const key = (byte) => new PublicKey(new Uint8Array(32).fill(byte));
const owner = key(1);
const market = key(2);
const positionId = key(3);
const baseMint = key(4);
const quoteMint = key(5);
const provider = {
  connection: {
    // The mint is Token-2022. A fixed Program<Token> account must still be
    // the legacy program; the handler selects the mint's actual program.
    getAccountInfo: async () => ({ owner: TOKEN_2022_PROGRAM_ID }),
  },
  publicKey: owner,
  wallet: { publicKey: owner },
};
const program = new anchor.Program(IDL, provider);
program.account.market.fetch = async () => ({
  baseSide: { assetMint: baseMint },
  quoteSide: { assetMint: quoteMint },
});
const write = new DuskWrite(program);

test("Token-2022 write helpers keep the fixed legacy Token account", async () => {
  const common = { market, owner, positionId, assetMint: baseMint, ownerAssetAccount: key(6) };
  const ylp = {
    market,
    owner,
    baseMint,
    quoteMint,
    ylpMint: key(7),
    ownerBaseAccount: key(8),
    ownerQuoteAccount: key(9),
    ownerYlpAccount: key(10),
  };
  const builds = [
    ["depositCollateral", () => write.depositCollateralInstruction({ ...common, depositAmount: 100n })],
    ["withdrawCollateral", () => write.withdrawCollateralInstruction({
      ...common, withdrawAmount: 1n, minAssetAmountOut: 0n, minLiquidationCfBps: 0,
    })],
    ["repay", () => write.repayInstruction({
      market, owner, positionId, debtAssetMint: baseMint, collateralAssetMint: quoteMint,
      ownerDebtAccount: key(11), repayAmount: 10n,
    })],
    ["addLiquidity", () => write.addLiquidityInstruction({
      ...ylp, baseDepositAmount: 100n, quoteDepositAmount: 100n, minYlpAmount: 0n,
    })],
    ["removeLiquidity", () => write.removeLiquidityInstruction({
      ...ylp, ylpAmount: 1n, minBaseAmountOut: 0n, minQuoteAmountOut: 0n,
    })],
  ];
  for (const [name, build] of builds) {
    const instruction = await build();
    const accounts = program.idl.instructions.find((candidate) => candidate.name === name).accounts;
    const tokenIndex = accounts.findIndex((account) => account.name === "tokenProgram");
    const token2022Index = accounts.findIndex((account) => account.name === "token2022Program");
    assert.ok(tokenIndex >= 0 && token2022Index >= 0, name);
    assert.equal(accounts[tokenIndex].address, TOKEN_PROGRAM_ID.toBase58(), name);
    assert.ok(instruction.keys[tokenIndex].pubkey.equals(TOKEN_PROGRAM_ID), name);
    assert.ok(instruction.keys[token2022Index].pubkey.equals(TOKEN_2022_PROGRAM_ID), name);
  }
});

test("governance execution resolves both market mint accounts", async () => {
  const proposal = key(12);
  const result = await write.executeParameterProposal({ market, proposal });
  const accounts = program.idl.instructions.find((candidate) => candidate.name === "executeParameterProposal").accounts;
  for (const [name, expected] of [["baseMint", baseMint], ["quoteMint", quoteMint]]) {
    const index = accounts.findIndex((account) => account.name === name);
    assert.ok(index >= 0, name);
    assert.ok(result.instruction.keys[index].pubkey.equals(expected), name);
  }
});
