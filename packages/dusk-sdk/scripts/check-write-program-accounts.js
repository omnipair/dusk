import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AddressLookupTableAccount, PublicKey, TransactionInstruction } from "@solana/web3.js";
import { marketCreationLookupTablePlan, marketCreationV0Transaction } from "../dist/market-launch.js";

import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";
import { defaultMarketLaunchConfig } from "../dist/market-bootstrap.js";
import { deriveTokenMetadataAddress, TOKEN_METADATA_PROGRAM_ID, MARKET_LAYOUT_VERSION } from "../dist/constants.js";

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
program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION,
  operator: {revenueRecipient:owner,revenueEpoch:new anchor.BN(0)},
  srlpMint: key(7),
  baseSide: { assetMint: baseMint },
  quoteSide: { assetMint: quoteMint },
  baseHlpVault: { srlpVault: key(13), hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0), srlpShares: new anchor.BN(0), debtShares: new anchor.BN(0) },
  quoteHlpVault: { srlpVault: key(14), hlpSupply: new anchor.BN(0), residualExposure: new anchor.BN(0), srlpShares: new anchor.BN(0), debtShares: new anchor.BN(0) },
});
program.account.operatorProposal.fetch=async()=>({revenueRecipient:null,nextOperator:null});
const write = new DuskWrite(program);

test("LP metadata correction pins the canonical PDA and makes no financial account writable", async () => {
  const lpMint = key(35), metadata = { name: "USDC hLP", symbol: "hUSDC", uri: "https://example.test/usdc.json" };
  const ix = await write.updateLpMetadataInstruction({ market, operator: owner, lpMint, metadata });
  const decoded = program.coder.instruction.decode(ix.data);
  assert.equal(decoded.name, "updateLpMetadata");
  assert.deepEqual(decoded.data.args, metadata);
  const spec = program.idl.instructions.find(row => row.name === decoded.name);
  const accounts = Object.fromEntries(spec.accounts.map((account, i) => [account.name, ix.keys[i]]));
  assert.equal(accounts.operator.pubkey.toBase58(), owner.toBase58());
  assert.equal(accounts.operator.isSigner, true);
  assert.equal(accounts.market.isWritable, false);
  assert.equal(accounts.lpMint.isWritable, false);
  assert.equal(accounts.tokenMetadataProgram.pubkey.toBase58(), TOKEN_METADATA_PROGRAM_ID.toBase58());
  assert.equal(accounts.tokenMetadata.pubkey.toBase58(), deriveTokenMetadataAddress(lpMint)[0].toBase58());
  assert.deepEqual(ix.keys.filter(key => key.isWritable).map(key => key.pubkey.toBase58()), [accounts.tokenMetadata.pubkey.toBase58()]);
  assert.throws(() => write.updateLpMetadataInstruction({ market, operator: owner, lpMint, metadata: { ...metadata, symbol: "x".repeat(11) } }), /10 bytes/);
});

function creationParams(oracle) {
  const metadata={name:"Omnipair Base hLP",symbol:"hLP",uri:"https://omnipair.fi/metadata/dusk/base-hlp.json"};
  return {payer:owner,baseMint,quoteMint,srlpMint:key(30),baseHlpMint:key(31),quoteHlpMint:key(32),
    teamTreasury:key(33),teamTreasuryWsolAccount:key(34),paramsHash:Array(32).fill(1),
    config:defaultMarketLaunchConfig(),oracle,lpMetadata:{srlp:metadata,baseHlp:metadata,quoteHlp:metadata}};
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
  const srlp = {
    market,
    owner,
    baseMint,
    quoteMint,
    srlpMint: key(7),
    ownerBaseAccount: key(8),
    ownerQuoteAccount: key(9),
    ownerSrlpAccount: key(10),
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
      ...srlp, baseDepositAmount: 100n, quoteDepositAmount: 100n, minSrlpAmount: 0n,
    })],
    ["removeLiquidity", () => write.removeLiquidityInstruction({
      ...srlp, srlpAmount: 1n, minBaseAmountOut: 0n, minQuoteAmountOut: 0n,
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

test("operator execution needs no LP governance, balance or voting accounts", async () => {
  const {DuskOperator}=await import("../dist/operator-client.js");
  const result=await new DuskOperator(program).execute({market,proposal:key(12)});
  const accounts=program.idl.instructions.find(ix=>ix.name==="executeOperatorChange").accounts;
  assert.deepEqual(accounts.map(account=>account.name),["market","futarchyAuthority","proposal","acceptingOperator","previousRevenue","eventAuthority","program"]);
  assert(result.instruction.keys[0].pubkey.equals(market));
  assert.equal(result.transaction.instructions.length,2);
});

const policy = (lambdaBps = 2500) => ({
  format: 1, provider: key(20), baseAccount: key(21), quoteAccount: PublicKey.default,
  baseFeedId: Array(32).fill(1), quoteFeedId: Array(32).fill(0), lambdaBps,
  adjustmentCapNad: new anchor.BN(2_000_000_000), maxAgeSeconds: 60,
  maxAgeSlots: new anchor.BN(100), maxConfidenceBps: 100, maxPublishSkewSeconds: 0,
});

test("oracle policy wire encoding matches Anchor and rejects ambiguous feed identities", async () => {
  const { encodeOraclePolicy, oraclePolicy, oracleReadAccounts, oracleUsesTwoFeeds, PYTH_RECEIVER_PROGRAM } = await import("../dist/oracle.js");
  for (const input of [policy(), {...policy(), format: 2, provider: PYTH_RECEIVER_PROGRAM,
    quoteAccount: key(22), quoteFeedId: Array(32).fill(2), maxPublishSkewSeconds: 3},
    {...policy(), format:3, provider:PYTH_RECEIVER_PROGRAM},
    {...policy(), format:4, quoteAccount:key(22),quoteFeedId:Array(32).fill(2),maxPublishSkewSeconds:3}]) {
    const ix = await write.initializeMarketInstruction(creationParams(input));
    const decoded=program.coder.instruction.decode(ix.data);
    assert.deepEqual(program.coder.types.encode("oraclePolicy",decoded.data.args.oracle), Buffer.from(encodeOraclePolicy(input)));
    assert.deepEqual(oracleReadAccounts(input).map(meta => meta.pubkey.toBase58()),
      [input.baseAccount, ...(oracleUsesTwoFeeds(input.format) ? [input.quoteAccount] : [])].map(key => key.toBase58()));
    assert.ok(oracleReadAccounts(input).every(meta => !meta.isWritable && !meta.isSigner));
  }
  assert.throws(() => oraclePolicy({...policy(), lambdaBps: 10_000}));
  assert.throws(() => oraclePolicy({...policy(), lambdaBps: 65_535}));
  for (const adjustmentCapNad of [0,999_999_999,1_000_000_000])
    assert.throws(() => oraclePolicy({...policy(), adjustmentCapNad}));
  assert.equal(oraclePolicy({...policy(),lambdaBps:9999,adjustmentCapNad:3_000_000_000}).lambdaBps,9999);
  assert.throws(() => oraclePolicy({...policy(), baseFeedId: [1]}));
  assert.deepEqual(oracleReadAccounts(policy(0)), []);
  assert.throws(() => oraclePolicy({...policy(),format:3}));
  assert.throws(() => oraclePolicy({...policy(),format:3,provider:PYTH_RECEIVER_PROGRAM,quoteAccount:key(22)}));
  assert.throws(() => oraclePolicy({...policy(),format:4}));
  assert.throws(() => oraclePolicy({...policy(),format:5}));
});

test("positive-weight previews append feeds after caller accounts and transaction budgets precede execution", async () => {
  const client = new anchor.Program(IDL, provider);
  const writer = new DuskWrite(client);
  client.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION,oracle: {policy: policy()}});
  const accounts = {market, futarchyAuthority: key(23), assetInMint: baseMint, assetOutMint: quoteMint};
  const remainingAccounts = [{pubkey:key(24), isSigner:false, isWritable:true}];
  const ix = await writer.instruction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.deepEqual(ix.keys.slice(-2).map(meta => meta.pubkey.toBase58()), [key(24),key(21)].map(key => key.toBase58()));
  const tx = await writer.transaction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.equal(tx.instructions.length, 3);
  assert.deepEqual(tx.instructions.slice(0,2).map(ix => ix.data[0]), [1,2]);
  assert.deepEqual(tx.instructions[2].data, ix.data);
  client.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION,oracle: {policy: policy(0)}});
  const native = await writer.transaction("previewSwap", {exactAssetIn: new anchor.BN(100)}, {accounts, remainingAccounts});
  assert.equal(native.instructions.length, 1);
  assert.equal(native.instructions[0].keys.at(-1).pubkey.toBase58(), key(24).toBase58());
});

test("transaction helpers and generic builds over oracle-reading instructions carry the feed and budget", async () => {
  const state = await program.account.market.fetch();
  for (const lambdaBps of [2500, 0]) {
    const client = new anchor.Program(IDL, provider);
    client.account.market.fetch = async () => ({ ...state, oracle: { policy: policy(lambdaBps) } });
    const writer = new DuskWrite(client);
    const lending = { market, owner, positionId, assetMint: baseMint, ownerAssetAccount: key(6) };
    const srlp = { market, owner, baseMint, quoteMint, srlpMint: key(7),
      ownerBaseAccount: key(8), ownerQuoteAccount: key(9), ownerSrlpAccount: key(10) };
    const transactions = await Promise.all([
      writer.depositCollateralTransaction({ ...lending, depositAmount: 100n }),
      writer.withdrawCollateralTransaction({ ...lending, withdrawAmount: 1n, minAssetAmountOut: 0n, minLiquidationCfBps: 0 }),
      writer.repayTransaction({ market, owner, positionId, debtAssetMint: baseMint, collateralAssetMint: quoteMint,
        ownerDebtAccount: key(11), repayAmount: 10n }),
      writer.addLiquidityTransaction({ ...srlp, baseDepositAmount: 100n, quoteDepositAmount: 100n, minSrlpAmount: 0n }),
      writer.removeLiquidityTransaction({ ...srlp, srlpAmount: 1n, minBaseAmountOut: 0n, minQuoteAmountOut: 0n }),
    ]);
    for (const tx of transactions) {
      const action = tx.instructions.at(-1);
      assert.ok(action.programId.equals(client.programId));
      assert.equal(action.keys.some(meta => meta.pubkey.equals(key(21))), lambdaBps > 0);
      // A positive weight needs the heap frame and unit limit ahead of the action.
      assert.deepEqual(tx.instructions.slice(0, -1).map(ix => ix.data[0]), lambdaBps > 0 ? [1, 2] : []);
      if (lambdaBps > 0) {
        assert.equal(tx.instructions[0].data.readUInt32LE(1), 256 * 1024);
        assert.equal(tx.instructions[1].data.readUInt32LE(1), 1_400_000);
      }
    }
    // compound_junior_seed binds the current oracle, so the generic builder
    // must append its feed like every other oracle-reading instruction.
    const seed = await writer.instruction("compoundJuniorSeed", undefined, { accounts: { market, juniorMint: key(26) } });
    assert.equal(seed.keys.some(meta => meta.pubkey.equals(key(21))), lambdaBps > 0);
  }
});

test("hLP liquidity actions preserve a caller-supplied oracle policy", async () => {
  const client = new anchor.Program(IDL, provider);
  const writer = new DuskWrite(client);
  const action = new TransactionInstruction({programId:client.programId,keys:[],data:Buffer.alloc(0)});
  writer.initializeYieldAccountsInstruction = async () => action;
  writer.instruction = async (name, _args, options) => {
    assert.equal(name,"depositSingleSided");
    assert.deepEqual(options.oraclePolicy,policy());
    assert.deepEqual(options.remainingAccounts.slice(0, 5).map(x => x.pubkey.toBase58()), [7, 13, 14, 15, 16].map(x => key(x).toBase58()));
    return action;
  };
  client.account.market.fetch = async () => { throw new Error("explicit snapshot and policy must avoid a market fetch"); };
  const result = await writer.depositSingleSided({}, {
    payer:owner,owner,market,targetHlpMint:key(25),baseMint,quoteMint,
    accounts:{},oraclePolicy:policy(),marketSnapshot: {
      srlpMint:key(7),
      baseSide:{assetMint:baseMint,interestVault:key(15),reserveVault:key(17)},
      quoteSide:{assetMint:quoteMint,interestVault:key(16),reserveVault:key(18)},
      baseHlpVault:{srlpVault:key(13),hlpSupply:new anchor.BN(0),residualExposure:new anchor.BN(0)},
      quoteHlpVault:{srlpVault:key(14),hlpSupply:new anchor.BN(0),residualExposure:new anchor.BN(0)},
    },
  });
  assert.equal(result.actionInstruction,action);
  assert.deepEqual(result.transaction.instructions.slice(0, 2).map(ix => ix.data[0]), [1, 2]);
  assert.equal(result.transaction.instructions[0].data.readUInt32LE(1), 256 * 1024);
  assert.equal(result.transaction.instructions[1].data.readUInt32LE(1), 1_400_000);
});

test("operator execution reads and budgets only the immutable market oracle", async () => {
  const {DuskOperator}=await import("../dist/operator-client.js");
  for(const current of [0,2500]) {
    const client=new anchor.Program(IDL,provider);
    client.account.market.fetch=async()=>({ version: MARKET_LAYOUT_VERSION,oracle:{policy:policy(current)},operator:{revenueRecipient:owner,revenueEpoch:new anchor.BN(0)}});
    client.account.operatorProposal.fetch=async()=>({revenueRecipient:null,nextOperator:null});
    const result=await new DuskOperator(client).execute({market,proposal:key(12)});
    assert.equal(result.transaction.instructions.length,current>0?3:2);
    assert.deepEqual(result.transaction.instructions.at(-1).data,result.instruction.data);
    assert.equal(result.instruction.keys.some(meta=>meta.pubkey.equals(key(21))),current>0);
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
  // Large enough to exceed Solana's packet limit while remaining below
  // Anchor's fixed 1,000-byte instruction-coder scratch buffer.
  const large={name:"n".repeat(32),symbol:"s".repeat(10),uri:"https://"+"u".repeat(110)};
  const oversized=await write.initializeMarketInstruction({...input,lpMetadata:{srlp:large,baseHlp:large,quoteHlp:large}});
  assert.throws(()=>build(oversized),/market creation exceeds transaction size/);
});
