import assert from "node:assert/strict";
import { createHash } from "node:crypto";

import anchor from "@coral-xyz/anchor";
import { PublicKey, AddressLookupTableAccount } from "@solana/web3.js";

import {
  anchorParameterUpdate,
  assertProposalTitle,
  assertProposalUri,
  canonicalParameterUpdates,
  centerControllerParameterUpdate,
  computeParameterProposalDigest,
  concentrationParameterUpdate,
  createProposalMetadata,
  dailyBorrowLimitParameterUpdate,
  emaHalfLivesParameterUpdate,
  feeParameterUpdate,
  insuranceDrawCapsParameterUpdate,
  oracleParameterUpdate,
  irmParameterUpdate,
  NAD,
  PARAMETER_PROPOSAL_DIGEST_DOMAIN,
  updatedFamilyRevisions,
  standardLaunchFeeParameterUpdate,
  resolveProposalDescriptionUri,
  tryFetchProposalDescription,
} from "../dist/governance.js";
import { deriveYieldAccountAddress } from "../dist/constants.js";
import IDL from "../dist/idl_v2.js";
import { DuskGovernance } from "../dist/governance-client.js";
import { deriveGovernanceBalanceAddress, deriveGovernanceSupportAddress } from "../dist/constants.js";
import { parameterProposalV0Transaction } from "../dist/market-launch.js";
import { DuskWrite } from "../dist/write.js";

const SOLANA_TRANSACTION_LIMIT = 1_232;
const EXPECTED_WORST_CASE_CREATE_SIZE = 1_345;
const { Program } = anchor;

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
program.account.market.fetch = async () => ({
  ylpMint: keys[2],
  baseSide: { assetMint: keys[3] },
  quoteSide: { assetMint: keys[4] },
  baseHlpVault: { ylpVault: keys[5] },
  quoteHlpVault: { ylpVault: keys[6] },
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
  oracleParameterUpdate({format:1,provider:keys[8],baseAccount:keys[9],quoteAccount:PublicKey.default,
    baseFeedId:Array(32).fill(7),quoteFeedId:Array(32).fill(0),lambdaBps:9999,adjustmentCapNad:2n*NAD,
    maxAgeSeconds:60,maxAgeSlots:100,maxConfidenceBps:100,maxPublishSkewSeconds:0}),
  insuranceDrawCapsParameterUpdate({ principalCoverageBps: 1_000, perDayBps: 3_000 }),
  centerControllerParameterUpdate({
    adjustmentThresholdNad: NAD / 100n,
    adjustmentStepNad: NAD / 1_000n,
    minAdjustmentIntervalSlots: 100,
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
    halfLifeMs: 14 * 24 * 60 * 60 * 1000,
    initialAprNad: 40_000_000,
    minimumAprNad: 20_000_000,
    maximumAprNad: 0,
    borrowUtilizationLimitBps: 8_000,
  }),
  concentrationParameterUpdate({
    peakAmplificationNad: 4n * NAD,
    coreHalfWidthBps: 100,
    fadeWidthBps: 400,
  }),
  update,
];
const canonical = canonicalParameterUpdates(everyFamily);
assert.deepEqual(
  canonical.map(({ kind }) => kind),
  ["fee", "concentration", "irm", "emaHalfLives", "dailyBorrowLimit", "centerController", "insuranceDrawCaps", "oracle"],
  "updates must be sent in ascending family order"
);
assert.throws(() => canonicalParameterUpdates([]), /at least one/);
assert.throws(
  () => canonicalParameterUpdates([dailyBorrowLimitParameterUpdate(1_000), dailyBorrowLimitParameterUpdate(2_000)]),
  /more than once/
);
assert.throws(() => insuranceDrawCapsParameterUpdate({ principalCoverageBps: 7_501, perDayBps: 5_000 }));

const marketRevisions = [11, 12, 13, 14, 15, 16, 17, 18];
assert.deepEqual(
  updatedFamilyRevisions([dailyBorrowLimitParameterUpdate(1_000)], marketRevisions),
  [0n, 0n, 0n, 0n, 15n, 0n, 0n, 0n],
  "families a proposal leaves alone bind revision zero"
);
const familyRevisions = updatedFamilyRevisions(everyFamily, marketRevisions);
const digestNonce = new anchor.BN(7);
const updatesBytes = Buffer.concat([
  Buffer.from(Uint32Array.of(canonical.length).buffer),
  ...canonical.map((value) =>
    program.coder.types.encode("marketParameterUpdate", anchorParameterUpdate(value))
  ),
]);
const metadataBytes = program.coder.types.encode("proposalMetadataV1", verifiedMetadata);
const u64Le = (value) => new anchor.BN(value.toString()).toArrayLike(Buffer, "le", 8);
assert.equal(PARAMETER_PROPOSAL_DIGEST_DOMAIN, "DUSK_PARAMETER_PROPOSAL_V2");
const expectedDigest = createHash("sha256")
  .update(
    Buffer.concat([
      Buffer.from(PARAMETER_PROPOSAL_DIGEST_DOMAIN),
      program.programId.toBuffer(),
      keys[1].toBuffer(),
      keys[0].toBuffer(),
      u64Le(digestNonce),
      ...familyRevisions.map(u64Le),
      updatesBytes,
      metadataBytes,
    ])
  )
  .digest();
const actualDigest = await computeParameterProposalDigest({
  programId: program.programId,
  market: keys[1],
  proposer: keys[0],
  nonce: digestNonce,
  familyRevisions,
  updates: everyFamily,
  metadata: verifiedMetadata,
});
assert.deepEqual(Buffer.from(actualDigest), expectedDigest, "proposal digest must match Anchor Borsh");

const build = await new DuskWrite(program).createParameterProposal({
  proposer: keys[0],
  market: keys[1],
  nonce: "18446744073709551615",
  updates: everyFamily,
  metadata,
  initialSupport: "18446744073709551615",
  holderYlpAccount: keys[7],
});
build.transaction.feePayer = keys[0];
build.transaction.recentBlockhash = keys[9].toBase58();
const legacySize = build.transaction.serializeMessage().length + 65;
assert.equal(legacySize, EXPECTED_WORST_CASE_CREATE_SIZE);
assert.ok(legacySize > SOLANA_TRANSACTION_LIMIT, "maximum proposal should exercise the v0 path");
const lookupTable = new AddressLookupTableAccount({key:keys[8],state:{
  deactivationSlot:0xffff_ffff_ffff_ffffn,lastExtendedSlot:0,lastExtendedSlotStartIndex:0,
  authority:keys[0],addresses:build.instruction.keys.filter(k=>!k.isSigner).map(k=>k.pubkey),
}});
const versioned = parameterProposalV0Transaction({payer:keys[0],recentBlockhash:keys[9].toBase58(),
  proposalInstruction:build.instruction,lookupTables:[lookupTable]});
const serializedSize = versioned.serialize().length;
assert.ok(serializedSize<=SOLANA_TRANSACTION_LIMIT);

const alternateProgramId = keys[8];
const alternateIdl = structuredClone(IDL);
alternateIdl.address = alternateProgramId.toBase58();
const alternateProgram = new Program(alternateIdl, provider);
alternateProgram.account.market.fetch = program.account.market.fetch;
const alternateWriter = new DuskWrite(alternateProgram);
const alternateBuild = await alternateWriter.createParameterProposal({
  proposer: keys[0],
  market: keys[1],
  nonce: 8,
  updates: [update],
  metadata: verifiedMetadata,
  initialSupport: 1,
  holderYlpAccount: keys[7],
});
const alternateBaseYield = deriveYieldAccountAddress(
  keys[1],
  keys[0],
  keys[2],
  keys[3],
  "ylp",
  alternateProgramId
)[0];
const alternateQuoteYield = deriveYieldAccountAddress(
  keys[1],
  keys[0],
  keys[2],
  keys[4],
  "ylp",
  alternateProgramId
)[0];
for (const expected of [alternateBaseYield, alternateQuoteYield]) {
  assert.ok(
    alternateBuild.instruction.keys.some(({ pubkey }) => pubkey.equals(expected)),
    "custom-program governance builders must derive yield PDAs with program.programId"
  );
}

console.log(
  `governance transaction-size check passed: ${serializedSize}/${SOLANA_TRANSACTION_LIMIT} bytes`
);

// Reusable governance builders preserve amounts alongside canonical sorting.
const governance = new DuskGovernance(program);
const balanceAddress = deriveGovernanceBalanceAddress(keys[1], keys[0], program.programId)[0];
const deposited = await governance.deposit({ market: keys[1], owner: keys[0], amount: 500n });
assert.equal(deposited.governanceBalance.toString(), balanceAddress.toString());
const depositDecoded = program.coder.instruction.decode(deposited.instruction.data);
assert.equal(depositDecoded.name, "manageGovernanceBalance");
assert.equal(depositDecoded.data.action.deposit.amount.toString(), "500");
const revoke = await governance.setDelegate({ market: keys[1], owner: keys[0], delegate: null });
assert.equal(program.coder.instruction.decode(revoke.instruction.data).data.delegate.toString(), PublicKey.default.toString());
const batch = await governance.createProposal({ market: keys[1], proposer: keys[7], nonce: 89,
  updates: [dailyBorrowLimitParameterUpdate(1900)], metadata: verifiedMetadata,
  contributions: [{ owner: keys[8], amount: 600 }, { owner: keys[0], amount: 700 }],
});
const batchDecoded = program.coder.instruction.decode(batch.instruction.data);
assert.equal(batchDecoded.name, "createGovernanceProposal");
assert.equal(batch.supports.length, 2);
assert(Buffer.compare(batch.supports[0].governanceBalance.toBuffer(), batch.supports[1].governanceBalance.toBuffer()) < 0);
for (const [index, support] of batch.supports.entries()) {
  assert.equal(batchDecoded.data.args.contributions[index].toString(), support.owner.equals(keys[8]) ? "600" : "700");
  assert.equal(support.governanceSupport.toString(), deriveGovernanceSupportAddress(batch.proposal, support.governanceBalance, program.programId)[0].toString());
}
await assert.rejects(governance.createProposal({ market: keys[1], proposer: keys[7], nonce: 90,
  updates: [dailyBorrowLimitParameterUpdate(1900)], metadata: verifiedMetadata,
  contributions: [{ owner: keys[0], amount: 600 }, { owner: keys[0], amount: 700 }],
}), /Duplicate governance owner/);
assert.throws(() => governance.deposit({ market: keys[1], owner: keys[0], amount: 0 }), /positive/);
const released = await governance.withdrawSupport({ market: keys[1], proposal: batch.proposal,
  owner: keys[0], authority: keys[7], amount: 50, proposer: keys[7], rentRefund: keys[7],
});
assert.equal(program.coder.instruction.decode(released.instruction.data).data.args.amount.toString(), "50");
console.log("Reusable governance builders: passed");

program.account.governanceSupport.all = async (filters) => {
  assert.equal(filters[0].memcmp.offset, 40);
  assert.equal(filters[0].memcmp.bytes, balanceAddress.toBase58());
  return [];
};
assert.deepEqual(await governance.listSupport(keys[1], keys[0]), []);
