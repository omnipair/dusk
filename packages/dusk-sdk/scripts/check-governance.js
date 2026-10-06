import assert from "node:assert/strict";
import { createHash } from "node:crypto";

import anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

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
import { DuskWrite } from "../dist/write.js";

const SOLANA_TRANSACTION_LIMIT = 1_232;
const EXPECTED_WORST_CASE_CREATE_SIZE = 1_123;
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
  insuranceDrawCapsParameterUpdate({ perEventBps: 1_000, perDayBps: 3_000 }),
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
    targetUtilizationBps: 6_500,
    curveSteepnessNad: 6n * NAD,
    adjustmentSpeedPerYear: 12,
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
  ["fee", "concentration", "irm", "emaHalfLives", "dailyBorrowLimit", "centerController", "insuranceDrawCaps"],
  "updates must be sent in ascending family order"
);
assert.throws(() => canonicalParameterUpdates([]), /at least one/);
assert.throws(
  () => canonicalParameterUpdates([dailyBorrowLimitParameterUpdate(1_000), dailyBorrowLimitParameterUpdate(2_000)]),
  /more than once/
);
assert.throws(() => insuranceDrawCapsParameterUpdate({ perEventBps: 2_001, perDayBps: 5_000 }));

const marketRevisions = [11, 12, 13, 14, 15, 16, 17];
assert.deepEqual(
  updatedFamilyRevisions([dailyBorrowLimitParameterUpdate(1_000)], marketRevisions),
  [0n, 0n, 0n, 0n, 15n, 0n, 0n],
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
const serializedSize = build.transaction.serialize({
  requireAllSignatures: false,
  verifySignatures: false,
}).length;

assert.equal(
  serializedSize,
  EXPECTED_WORST_CASE_CREATE_SIZE,
  "worst-case create-parameter-proposal transaction ABI size changed"
);
assert.ok(
  serializedSize <= SOLANA_TRANSACTION_LIMIT,
  `worst-case proposal creation is ${serializedSize} bytes, above ${SOLANA_TRANSACTION_LIMIT}`
);

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
