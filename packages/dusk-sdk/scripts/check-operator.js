import assert from "node:assert/strict";
import {existsSync} from "node:fs";
import { createHash } from "node:crypto";

import anchor from "@coral-xyz/anchor";
import { PublicKey, AddressLookupTableAccount } from "@solana/web3.js";

import {
  anchorParameterUpdate, assertProposalTitle, assertProposalUri, canonicalParameterUpdates,
  centerControllerParameterUpdate, concentrationParameterUpdate, createProposalMetadata,
  dailyBorrowLimitParameterUpdate, emaHalfLivesParameterUpdate, feeParameterUpdate,
  irmParameterUpdate, NAD, standardLaunchFeeParameterUpdate, trancheParameterUpdate,
  resolveProposalDescriptionUri, tryFetchProposalDescription,
} from "../dist/parameters.js";
import IDL from "../dist/idl_v2.js";
import {DuskOperator} from "../dist/operator-client.js";
import {deriveOperatorProposalAddress,OPERATOR_NOTICE_SLOTS,JUNIOR_MAX_NOTICE_SLOTS,MARKET_LAYOUT_VERSION} from "../dist/constants.js";
import {operatorChangeV0Transaction} from "../dist/market-launch.js";
const {Program}=anchor;
const keys = Array.from(
  { length: 10 },
  (_, index) =>
    new PublicKey(
      Uint8Array.from({ length: 32 }, (_, byte) => (index * 33 + byte + 1) % 256)
    )
);
const provider = {
  connection: {},
  wallet: { publicKey: keys[0] },
  publicKey: keys[0],
};
const program = new Program(IDL, provider);
program.account.market.fetch = async () => ({ version: MARKET_LAYOUT_VERSION,
  srlpMint: keys[2],
  juniorMint: keys[9],
  baseSide: { assetMint: keys[3] },
  quoteSide: { assetMint: keys[4] },
  baseHlpVault: { srlpVault: keys[5] },
  quoteHlpVault: { srlpVault: keys[6] },
});

const rationaleBytes = new TextEncoder().encode("# Rationale\nExact bytes matter.\n");
const rationaleFetch = async () =>
  new Response(rationaleBytes, {
    headers: { "content-length": String(rationaleBytes.length) },
  });
const verifiedMetadata = await createProposalMetadata({
  title: "Raise the daily borrow limit",
  markdown: rationaleBytes,
  descriptionUri: "https://example.invalid/proposal.md",
  fetch: rationaleFetch,
});
assert.equal(verifiedMetadata.descriptionLen, rationaleBytes.length);
assert.equal(
  (await tryFetchProposalDescription(verifiedMetadata, { fetch: rationaleFetch })).verified,
  true
);
const alteredBytes = rationaleBytes.slice();
alteredBytes[alteredBytes.length - 2] ^= 1;
const alteredResult = await tryFetchProposalDescription(verifiedMetadata, {
  fetch: async () =>
    new Response(alteredBytes, {
      headers: { "content-length": String(alteredBytes.length) },
    }),
});
assert.equal(alteredResult.verified, false, "a one-byte rationale change must fail SHA-256");
assert.throws(() => assertProposalTitle("T".repeat(97)));
assert.throws(() => assertProposalUri("http://example.invalid/proposal.md"));
assert.equal(
  resolveProposalDescriptionUri("ipfs://bafy-test/path.md"),
  "https://ipfs.io/ipfs/bafy-test/path.md"
);
assert.equal(
  resolveProposalDescriptionUri("ar://transaction-id"),
  "https://arweave.net/transaction-id"
);

const metadata = {
  version: 1,
  title: "T".repeat(96),
  descriptionUri: `https://${"a".repeat(192)}`,
  descriptionSha256: [1, ...Array(31).fill(0)],
  descriptionLen: 32_768,
};
const update = feeParameterUpdate({
  baseFeeBps: 5_000,
  divergenceFeeShareCapBps: 0,
  volatilityFeeShareCapBps: 0,
  divergenceFeeCoefficientNad: 100_000_000_000n,
  volatilityFeeCoefficientNad: 100_000_000_000n,
  volatilityHalfLifeMs: 43_200_000,
  volatilityShockCapNad: 10_000_000_000n,
  volatilityAccumulatorCapNad: 10_000_000_000n,
  swapFeeCollectMode: 0,
  compoundingFeeBps: 4_000,
});
const launchUpdate = feeParameterUpdate({
  baseFeeBps: 30,
  divergenceFeeShareCapBps: 1_500,
  volatilityFeeShareCapBps: 1_500,
  divergenceFeeCoefficientNad: 0,
  volatilityFeeCoefficientNad: 0,
  volatilityHalfLifeMs: 60_000,
  volatilityShockCapNad: 0,
  volatilityAccumulatorCapNad: 0,
  swapFeeCollectMode: 2,
  compoundingFeeBps: 4_000,
  launchFeeStartBps: 500,
  launchFeeDurationSeconds: 3_600,
  launchFeeDecayMode: 1,
  launchMarketPriceStepBps: 1_000,
  launchMarketNumberOfPeriods: 8,
  launchMarketReductionFactorBps: 1_000,
  launchRateLimitAsset: 1,
  launchRateLimitReferenceNad: 100_000_000_000n,
  launchRateLimitIncrementBps: 100,
  launchRateLimitMaxFeeBps: 2_000,
  launchRateLimitDurationSeconds: 3_600,
});
assert.equal(launchUpdate.profile.launchFeeStartBps, 500);
assert.equal(launchUpdate.profile.swapFeeCollectMode, 2);
assert.equal(launchUpdate.profile.compoundingFeeBps, 4_000);
assert.equal(launchUpdate.profile.launchMarketNumberOfPeriods, 8);
assert.equal(launchUpdate.profile.launchRateLimitAsset, 1);
const standardLaunch = standardLaunchFeeParameterUpdate({
  baseFeeBps: 30,
  divergenceFeeShareCapBps: 0,
  volatilityFeeShareCapBps: 0,
  divergenceFeeCoefficientNad: 0,
  volatilityFeeCoefficientNad: 0,
  volatilityHalfLifeMs: 60_000,
  volatilityShockCapNad: 0,
  volatilityAccumulatorCapNad: 0,
  compoundingFeeBps: 0,
  launchFeeStartBps: 500,
  launchFeeDurationSeconds: 3_600,
  launchFeeDecayMode: 1,
  launchRateLimitReferenceNad: 100_000_000_000n,
  launchRateLimitIncrementBps: 100,
  launchRateLimitMaxFeeBps: 2_000,
  launchRateLimitDurationSeconds: 3_600,
});
assert.equal(standardLaunch.profile.swapFeeCollectMode, 2);
assert.equal(standardLaunch.profile.launchRateLimitAsset, 1);
assert.equal(standardLaunch.profile.compoundingFeeBps, 0);
assert.throws(() =>
  feeParameterUpdate({
    baseFeeBps: 30,
    divergenceFeeShareCapBps: 0,
    volatilityFeeShareCapBps: 0,
    divergenceFeeCoefficientNad: 0,
    volatilityFeeCoefficientNad: 0,
    volatilityHalfLifeMs: 60_000,
    volatilityShockCapNad: 0,
    volatilityAccumulatorCapNad: 0,
    compoundingFeeBps: 10_001,
  })
);
assert.throws(() =>
  feeParameterUpdate({
    baseFeeBps: 30,
    divergenceFeeShareCapBps: 0,
    volatilityFeeShareCapBps: 0,
    divergenceFeeCoefficientNad: 0,
    volatilityFeeCoefficientNad: 0,
    volatilityHalfLifeMs: 60_000,
    volatilityShockCapNad: 0,
    volatilityAccumulatorCapNad: 0,
    launchRateLimitAsset: 1,
  })
);
// One update per family, deliberately out of order: the SDK sends them in
// ascending family order, the only order the program accepts.
const everyFamily = [
  centerControllerParameterUpdate({
    adjustmentStepNad: NAD / 1_000n,
  }),
  dailyBorrowLimitParameterUpdate(3_000),
  emaHalfLivesParameterUpdate({
    priceMs: 120_000,
    directionalPriceMs: 180_000,
    curveDepthMs: 240_000,
    centerPriceMs: 300_000,
  }),
  irmParameterUpdate({
    lowerSignalNad: 50_000_000,
    upperSignalNad: 62_500_000,
    lowerUtilizationBps: 7_500,
    upperUtilizationBps: 8_000,
    halfLifeMs: 14 * 24 * 60 * 60 * 1000,
  }),
  concentrationParameterUpdate({
    peakAmplificationNad: 4n * NAD,
    coreHalfWidthBps: 100,
    fadeWidthBps: 400,
  }),
  trancheParameterUpdate({
    juniorIncomeShareBps: 5_000,

  }),
  update,
];
const canonical = canonicalParameterUpdates(everyFamily);
assert.deepEqual(
  canonical.map(({ kind }) => kind),
  ["fee", "concentration", "irm", "emaHalfLives", "dailyBorrowLimit", "centerController", "tranche"],
  "updates must be sent in ascending family order"
);
assert.throws(() => canonicalParameterUpdates([]), /at least one/);
assert.throws(
  () => canonicalParameterUpdates([dailyBorrowLimitParameterUpdate(1_000), dailyBorrowLimitParameterUpdate(2_000)]),
  /more than once/
);


// Exact on-chain mutable surface: authority is independent of token balances.
for(const name of ["governance","governance-client","management"])assert.equal(existsSync(new URL(`../dist/${name}.js`,import.meta.url)),false,`retired ${name} output must not ship`);
assert(!IDL.instructions.some(ix=>/governance|parameter_proposal|recovery_ballot/i.test(ix.name)));
assert(!IDL.types.some(type=>/Governance|RecoveryBallot|ProposalSupport|ParameterProposal/.test(type.name)));
const configType=IDL.types.find(type=>type.name==="RiskRateConfig");
assert.deepEqual(configType.type.fields.map(field=>field.name),[
  "lower_signal_nad","upper_signal_nad","lower_utilization_bps","upper_utilization_bps","half_life_ms","initial_apr_nad"]);
const updateType=IDL.types.find(type=>type.name==="RiskRateUpdate");
assert(!updateType.type.fields.some(field=>/initial|minimum|maximum/.test(field.name)));
assert.equal(OPERATOR_NOTICE_SLOTS,JUNIOR_MAX_NOTICE_SLOTS+432_000);
const validIrm={lowerSignalNad:50_000_000,upperSignalNad:62_500_000,lowerUtilizationBps:7_500,upperUtilizationBps:8_000,halfLifeMs:43_200_000};
for(const halfLifeMs of [0,43_199_999,30*86_400_000+1])assert.throws(()=>irmParameterUpdate({...validIrm,halfLifeMs}));
for(const halfLifeMs of [43_200_000,30*86_400_000])assert.doesNotThrow(()=>irmParameterUpdate({...validIrm,halfLifeMs}));
assert.throws(()=>irmParameterUpdate({...validIrm,initialAprNad:40_000_000}),/Unknown/);
assert.throws(()=>irmParameterUpdate({...validIrm,lowerUtilizationBps:8_000}));
assert.throws(()=>irmParameterUpdate({...validIrm,upperUtilizationBps:10_000}));
assert.throws(()=>concentrationParameterUpdate({peakAmplificationNad:NAD,coreHalfWidthBps:0,fadeWidthBps:0,rampDurationSlots:431_999}));
assert.throws(()=>centerControllerParameterUpdate({adjustmentStepNad:0}));

const operatorState={authority:keys[0],manager:PublicKey.default,pendingAuthor:PublicKey.default,epoch:new anchor.BN(7),nextNonce:new anchor.BN(9),pending:PublicKey.default,
  revenueRecipient:keys[0],interestShareBps:0};
program.account.market.fetch=async()=>({ version: MARKET_LAYOUT_VERSION,operator:operatorState});
const client=new DuskOperator(program);
const announced=await client.announce({market:keys[1],payer:keys[0],operator:keys[7],metadata:verifiedMetadata,
  updates:canonical,nextManager:keys[6],revenueRecipient:keys[9],interestShareBps:5_000});
assert(announced.proposal.equals(deriveOperatorProposalAddress(keys[1],9,program.programId)[0]));
const decoded=program.coder.instruction.decode(announced.instruction.data);
assert.equal(decoded.name,"announceOperatorChange");
assert.equal(decoded.data.args.expectedEpoch.toString(),"7");
assert.equal(decoded.data.args.updates.length,7);
assert(decoded.data.args.nextManager.equals(keys[6]));
assert(announced.instruction.keys.some(meta=>meta.pubkey.equals(keys[0])&&meta.isSigner));
assert(announced.instruction.keys.some(meta=>meta.pubkey.equals(keys[7])&&meta.isSigner));
await assert.rejects(()=>client.announce({market:keys[1],payer:keys[0],operator:keys[0],metadata:verifiedMetadata}),/at least/);
await assert.rejects(()=>client.announce({market:keys[1],payer:keys[0],operator:keys[0],metadata:verifiedMetadata,interestShareBps:5_001}),/0–5000/);
const lookupTable=new AddressLookupTableAccount({key:keys[8],state:{deactivationSlot:0xffff_ffff_ffff_ffffn,lastExtendedSlot:0,
  lastExtendedSlotStartIndex:0,authority:keys[0],addresses:announced.instruction.keys.filter(meta=>!meta.isSigner).map(meta=>meta.pubkey)}});
const versioned=operatorChangeV0Transaction({payer:keys[0],recentBlockhash:keys[9].toBase58(),proposalInstruction:announced.instruction,lookupTables:[lookupTable]});
assert(versioned.serialize().length<=1_232);

await assert.rejects(()=>client.announce({market:keys[1],payer:keys[0],operator:keys[0],metadata:verifiedMetadata,nextOperator:keys[8],nextManager:keys[6]}),/clears the manager/);
const removal=await client.removeManager({market:keys[1],operator:keys[0]});
assert.equal(program.coder.instruction.decode(removal.instruction.data).name,"removeMarketManager");
