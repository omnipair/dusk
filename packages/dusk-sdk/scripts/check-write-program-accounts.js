import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AddressLookupTableAccount, PublicKey } from "@solana/web3.js";
import { marketCreationLookupTablePlan, marketCreationV0Transaction } from "../dist/market-launch.js";

import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";
import { defaultMarketLaunchConfig } from "../dist/market-bootstrap.js";

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

function creationParams(oracle) {
  const metadata={name:"Omnipair Base hLP",symbol:"hLP",uri:"https://omnipair.fi/metadata/dusk/base-hlp.json"};
  return {payer:owner,baseMint,quoteMint,ylpMint:key(30),baseHlpMint:key(31),quoteHlpMint:key(32),
    teamTreasury:key(33),teamTreasuryWsolAccount:key(34),paramsHash:Array(32).fill(1),
    config:defaultMarketLaunchConfig(),oracle,lpMetadata:{ylp:metadata,baseHlp:metadata,quoteHlp:metadata}};
}

test("market initialization defaults to native and accepts an explicit null oracle", async () => {
  for (const oracle of [undefined,null]) {
    const ix=await write.initializeMarketInstruction(creationParams(oracle));
    const decoded=program.coder.instruction.decode(ix.data);
    assert.equal(decoded.name,"initializeMarket");
    assert.equal(decoded.data.args.oracle,null);
    assert.equal(decoded.data.args.bootstrapPriceNad.toString(),"0");
    assert.deepEqual([...ix.data.subarray(8,40)],Array(32).fill(1));
    const initialize = program.idl.instructions.find(instruction => instruction.name === "initializeMarket");
    const accounts = Object.fromEntries(initialize.accounts.map((account, i) => [account.name, ix.keys[i].pubkey]));
    delete accounts.market;
    const resolved = await program.methods.initializeMarket(decoded.data.args).accountsPartial(accounts).instruction();
    assert.deepEqual(resolved.keys, ix.keys);
  }
  assert.equal(program.idl.instructions.some(ix=>ix.name==="configureInitialOracle"),false);
});

test("guided creation cannot supply a second, independent opening price", async () => {
  const oracle={format:1,provider:key(40),baseAccount:key(41),quoteAccount:PublicKey.default,
    baseFeedId:Array(32).fill(1),quoteFeedId:Array(32).fill(0),lambdaBps:3500,
    adjustmentCapNad:"1250000000",maxAgeSeconds:30,maxAgeSlots:"75",maxConfidenceBps:100,maxPublishSkewSeconds:0};
  await assert.rejects(write.initializeMarketInstruction({...creationParams(oracle),bootstrapPriceNad:1_000_000_000}),
    /derive the initial price from the feed/);
  const guided=await write.initializeMarketInstruction(creationParams(oracle));
  assert.equal(program.coder.instruction.decode(guided.data).data.args.bootstrapPriceNad.toString(),"0");
  const native=await write.initializeMarketInstruction({...creationParams(null),bootstrapPriceNad:1_000_000_000});
  assert.equal(program.coder.instruction.decode(native.data).data.args.bootstrapPriceNad.toString(),"1000000000");
});

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
    const ix = await write.initializeMarketInstruction(creationParams(input));
    const decoded=program.coder.instruction.decode(ix.data);
    assert.deepEqual(program.coder.types.encode("oraclePolicy",decoded.data.args.oracle), Buffer.from(encodeOraclePolicy(input)));
    assert.deepEqual(oracleReadAccounts(input).map(meta => meta.pubkey.toBase58()),
      [input.baseAccount, ...(input.format === 2 ? [input.quoteAccount] : [])].map(key => key.toBase58()));
    assert.ok(oracleReadAccounts(input).every(meta => !meta.isWritable && !meta.isSigner));
  }
  assert.throws(() => oraclePolicy({...policy(), lambdaBps: 10_000}));
  assert.throws(() => oraclePolicy({...policy(), lambdaBps: 65_535}));
  for (const adjustmentCapNad of [0,999_999_999,1_000_000_000])
    assert.throws(() => oraclePolicy({...policy(), adjustmentCapNad}));
  assert.equal(oraclePolicy({...policy(),lambdaBps:9999,adjustmentCapNad:3_000_000_000}).lambdaBps,9999);
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

test("governance execution budgets activation and disable without requiring the old feed", async () => {
  for (const [current, next, expectedInstructions] of [[0,2500,3],[2500,0,3],[0,0,1]]) {
    const client = new anchor.Program(IDL, provider);
    client.account.market.fetch = async () => ({baseSide:{assetMint:baseMint},quoteSide:{assetMint:quoteMint},
      oracle:{policy:policy(current)}});
    client.account.parameterProposal.fetch = async () => ({updates:[{oracle:{0:policy(next)}}]});
    const result = await new DuskWrite(client).executeParameterProposal({market,proposal:key(12)});
    assert.equal(result.transaction.instructions.length,expectedInstructions);
    if (expectedInstructions===3)
      assert.deepEqual(result.transaction.instructions.slice(0,2).map(ix=>ix.data[0]),[1,2]);
    assert.deepEqual(result.transaction.instructions.at(-1).data,result.instruction.data);
    assert.equal(result.instruction.keys.some(meta=>meta.pubkey.equals(key(21))),next>0);
  }
});

test("oracle-selected creation fits a versioned transaction and rejects oversized metadata", async () => {
  const input=creationParams(policy());
  const ix=await write.initializeMarketInstruction(input);
  const plan=marketCreationLookupTablePlan({payer:owner,recentSlot:100,initializeMarketInstruction:ix});
  const lookupTable=new AddressLookupTableAccount({key:plan.address,state:{
    deactivationSlot:0xffff_ffff_ffff_ffffn,lastExtendedSlot:100,lastExtendedSlotStartIndex:0,
    authority:owner,addresses:plan.addresses,
  }});
  const build=initializeMarketInstruction=>marketCreationV0Transaction({payer:owner,recentBlockhash:key(40).toBase58(),
    initializeMarketInstruction,lookupTable});
  assert.ok(build(ix).serialize().length<=1232);
  const large={name:"n".repeat(32),symbol:"s".repeat(10),uri:"https://"+"u".repeat(127)};
  const oversized=await write.initializeMarketInstruction({...input,lpMetadata:{ylp:large,baseHlp:large,quoteHlp:large}});
  assert.throws(()=>build(oversized),/market creation exceeds transaction size/);
});
