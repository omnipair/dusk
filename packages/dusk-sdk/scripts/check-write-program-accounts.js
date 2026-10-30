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
  baseHlpVault: { hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0), ylpShares: new anchor.BN(0), debtShares: new anchor.BN(0) },
  quoteHlpVault: { hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0), ylpShares: new anchor.BN(0), debtShares: new anchor.BN(0) },
});
program.account.parameterProposal.fetch = async () => ({ updates: [] });
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

const policy = (lambdaBps = 2500) => ({
  format: 1, provider: key(20), baseAccount: key(21), quoteAccount: PublicKey.default,
  baseFeedId: Array(32).fill(1), quoteFeedId: Array(32).fill(0), lambdaBps,
  adjustmentCapNad: new anchor.BN(2_000_000_000), maxAgeSeconds: 60,
  maxAgeSlots: new anchor.BN(100), maxConfidenceBps: 100, maxPublishSkewSeconds: 0,
});

test("oracle policy wire encoding matches Anchor and rejects ambiguous feed identities", async () => {
  const { encodeOraclePolicy, oraclePolicy, oracleReadAccounts, PYTH_RECEIVER_PROGRAM } = await import("../dist/oracle.js");
  for (const input of [policy(), {...policy(), format: 2, provider: PYTH_RECEIVER_PROGRAM,
    quoteAccount: key(22), quoteFeedId: Array(32).fill(2), maxPublishSkewSeconds: 3}]) {
    const ix = await write.configureInitialOracleInstruction({market, authority: owner, policy: input});
    assert.deepEqual(ix.data.subarray(8), Buffer.from(encodeOraclePolicy(input)));
    assert.deepEqual(oracleReadAccounts(input).map(meta => meta.pubkey.toBase58()),
      [input.baseAccount, ...(input.format === 2 ? [input.quoteAccount] : [])].map(key => key.toBase58()));
    assert.ok(oracleReadAccounts(input).every(meta => !meta.isWritable && !meta.isSigner));
  }
  assert.throws(() => oraclePolicy({...policy(), lambdaBps: 10_000}));
  assert.throws(() => oraclePolicy({...policy(), baseFeedId: [1]}));
  assert.deepEqual(oracleReadAccounts(policy(0)), []);
});

test("positive-weight previews append feeds after caller accounts and transaction budgets precede execution", async () => {
  const client = new anchor.Program(IDL, provider);
  const writer = new DuskWrite(client);
  client.account.market.fetch = async () => ({oracle: {policy: policy()}});
  const accounts = {market, futarchyAuthority: key(23), assetInMint: baseMint, assetOutMint: quoteMint};
  const remainingAccounts = [{pubkey:key(24), isSigner:false, isWritable:true}];
  const ix = await writer.instruction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.deepEqual(ix.keys.slice(-2).map(meta => meta.pubkey.toBase58()), [key(24),key(21)].map(key => key.toBase58()));
  const tx = await writer.transaction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.equal(tx.instructions.length, 3);
  assert.deepEqual(tx.instructions.slice(0,2).map(ix => ix.data[0]), [1,2]);
  assert.deepEqual(tx.instructions[2].data, ix.data);
  client.account.market.fetch = async () => ({oracle: {policy: policy(0)}});
  const native = await writer.transaction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.equal(native.instructions.length, 1);
  assert.equal(native.instructions[0].keys.at(-1).pubkey.toBase58(), key(24).toBase58());
});
