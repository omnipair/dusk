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
  computeDecodedExecutionDigest,
  verifyDecodedExecutionDigest,
  anchorGovernanceAction,
  anchorRevisionConditions,
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
  trancheParameterUpdate,
  resolveProposalDescriptionUri,
  tryFetchProposalDescription,
} from "../dist/governance.js";
import { deriveYieldAccountAddress } from "../dist/constants.js";
import IDL from "../dist/idl_v2.js";
import { DuskGovernance } from "../dist/governance-client.js";
import { deriveGovernanceBalanceAddress, deriveGovernanceSupportAddress, deriveGovernanceControlAddress, deriveCancellationBallotAddress, deriveRemovalBallotAddress } from "../dist/constants.js";
import { deriveRecoveryBallotAddress, deriveRecoveryVoteAddress, decodeProposalOutcome, MANAGER_POLICY_VERSION, MANAGER_PENDING_LIMIT, MANAGER_ANNOUNCEMENT_COOLDOWN_SECONDS, RECOVERY_BALLOT_WINDOW_SECONDS, JUNIOR_MAX_NOTICE_SLOTS, MANAGER_NOTICE_SLOTS, MANAGER_EXECUTION_WINDOW_SLOTS, proposalClockUnit } from "../dist/management.js";
import { parameterProposalV0Transaction } from "../dist/market-launch.js";
import { DuskWrite } from "../dist/write.js";

const SOLANA_TRANSACTION_LIMIT = 1_232;
const EXPECTED_WORST_CASE_CREATE_SIZE = 1_390;
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
  trancheParameterUpdate({
    juniorIncomeShareBps: 5_000,
    requestFeeBps: 5,
    liquidationFeeBps: 100,
  }),
  update,
];
const canonical = canonicalParameterUpdates(everyFamily);
assert.deepEqual(
  canonical.map(({ kind }) => kind),
  ["fee", "concentration", "irm", "emaHalfLives", "dailyBorrowLimit", "centerController", "insuranceDrawCaps", "oracle", "tranche"],
  "updates must be sent in ascending family order"
);
assert.throws(() => canonicalParameterUpdates([]), /at least one/);
assert.throws(
  () => canonicalParameterUpdates([dailyBorrowLimitParameterUpdate(1_000), dailyBorrowLimitParameterUpdate(2_000)]),
  /more than once/
);
assert.throws(() => insuranceDrawCapsParameterUpdate({ principalCoverageBps: 7_501, perDayBps: 5_000 }));

const marketRevisions = [11, 12, 13, 14, 15, 16, 17, 18, 19];
assert.deepEqual(
  updatedFamilyRevisions([dailyBorrowLimitParameterUpdate(1_000)], marketRevisions),
  [0n, 0n, 0n, 0n, 15n, 0n, 0n, 0n, 0n],
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
assert.equal(PARAMETER_PROPOSAL_DIGEST_DOMAIN, "DUSK_PARAMETER_PROPOSAL_V4");
const expectedDigest = createHash("sha256")
  .update(
    Buffer.concat([
      Buffer.from(PARAMETER_PROPOSAL_DIGEST_DOMAIN),
      program.programId.toBuffer(),
      keys[1].toBuffer(),
      keys[0].toBuffer(),
      u64Le(digestNonce),
      u64Le(0),
      Buffer.from([0]), // LP origin
      Buffer.from([0]), // parameter action
      Buffer.alloc(4), // no explicitly endorsed revision conditions
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
  juniorGeneration: 0,
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
  holderSrlpAccount: keys[7],
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
  holderSrlpAccount: keys[7],
});
const alternateBaseYield = deriveYieldAccountAddress(
  keys[1],
  keys[0],
  keys[2],
  keys[3],
  "srlp",
  alternateProgramId
)[0];
const alternateQuoteYield = deriveYieldAccountAddress(
  keys[1],
  keys[0],
  keys[2],
  keys[4],
  "srlp",
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
const balanceAddress = deriveGovernanceBalanceAddress(keys[1], keys[0], keys[2], program.programId)[0];
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
}), /Duplicate governance balance/);
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

// Historical governance selection preserves the original mint and cohort.
const retiredMint = keys[8];
const retiredBalance = deriveGovernanceBalanceAddress(keys[1], keys[0], retiredMint, program.programId)[0];
const cleanup = await governance.withdraw({ market: keys[1], owner: keys[0], kind: "junior", lpMint: retiredMint, amount: 5 });
assert.equal(cleanup.governanceBalance.toString(), retiredBalance.toString());
assert.equal(cleanup.instruction.keys[cleanup.instruction.keys.length - 1].pubkey.toString(),
  PublicKey.findProgramAddressSync([Buffer.from("junior_cohort"), keys[1].toBuffer(), retiredMint.toBuffer()], program.programId)[0].toString());
const oldRelease = await governance.withdrawSupport({ market: keys[1], proposal: batch.proposal, owner: keys[0],
  authority: keys[7], kind: "junior", lpMint: retiredMint, proposer: keys[7], rentRefund: keys[7] });
assert.equal(oldRelease.governanceBalance.toString(), retiredBalance.toString());
const oldRevoke = await governance.setDelegate({ market: keys[1], owner: keys[0], kind: "junior", lpMint: retiredMint, delegate: null });
assert.equal(oldRevoke.governanceBalance.toString(), retiredBalance.toString());
const nextGenerationDigest = await computeParameterProposalDigest({ programId: program.programId, market: keys[1], proposer: keys[0],
  nonce: digestNonce, juniorGeneration: 1, familyRevisions, updates: everyFamily, metadata: verifiedMetadata });
assert.notDeepEqual(Buffer.from(nextGenerationDigest), expectedDigest);

// Starting settings do not alter an endorsement; explicit conditions do.
const intentInput = {programId: program.programId, market: keys[1], proposer: keys[0], nonce: 7,
  juniorGeneration: 0, updates: [dailyBorrowLimitParameterUpdate(1_900)], metadata: verifiedMetadata};
const stableIntent = await computeParameterProposalDigest({...intentInput, familyRevisions: marketRevisions});
assert.deepEqual(await computeParameterProposalDigest({...intentInput, familyRevisions: marketRevisions.map(value => value + 1)}), stableIntent);
assert.notDeepEqual(await computeParameterProposalDigest({...intentInput, conditions: [{family: 4, revision: 15}]}), stableIntent);
await assert.rejects(computeParameterProposalDigest({...intentInput, conditions: [{family: 4, revision: 15}, {family: 4, revision: 16}]}), /duplicate/);

const origin = {kind: "manager", epoch: 7, policyVersion: 1};
const conditions = [{family: 4, revision: 15}];
const managedIntent = await computeParameterProposalDigest({...intentInput, origin, conditions});
const encodedConditions = anchorRevisionConditions(conditions);
const encodedUpdates = intentInput.updates.map(anchorParameterUpdate);
const expectedManagedIntent = createHash("sha256").update(Buffer.concat([
  Buffer.from(PARAMETER_PROPOSAL_DIGEST_DOMAIN), program.programId.toBuffer(), keys[1].toBuffer(), keys[0].toBuffer(),
  u64Le(7), u64Le(0), program.coder.types.encode("proposalOrigin", {manager: {epoch: new anchor.BN(7), policyVersion: new anchor.BN(1)}}),
  program.coder.types.encode("governanceAction", anchorGovernanceAction()),
  Buffer.from([1, 0, 0, 0]), program.coder.types.encode("familyRevisionCondition", encodedConditions[0]),
  Buffer.from([1, 0, 0, 0]), program.coder.types.encode("marketParameterUpdate", encodedUpdates[0]), metadataBytes,
])).digest();
assert.deepEqual(Buffer.from(managedIntent), expectedManagedIntent);
const decoded = {market: keys[1], proposer: keys[0], nonce: new anchor.BN(7), juniorGeneration: new anchor.BN(0),
  origin: {manager: {epoch: new anchor.BN(7), policyVersion: new anchor.BN(1)}}, action: {parameters: {}},
  conditions: encodedConditions, updates: encodedUpdates, metadata: verifiedMetadata, digest: managedIntent,
  familyRevisions: [0, 0, 0, 0, 15, 0, 0, 0, 0].map(value => new anchor.BN(value)),
  queuedSupport: new anchor.BN(0), queuedEligibleSrlp: new anchor.BN(0), queuedAt: new anchor.BN(100),
  executeAfter: new anchor.BN(604_900), executionDeadline: new anchor.BN(1_209_700)};
const expectedExecution = createHash("sha256").update(Buffer.concat([
  Buffer.from("DUSK_PARAMETER_EXECUTION_V2"), expectedManagedIntent,
  ...decoded.familyRevisions.map(u64Le), u64Le(0), u64Le(0), u64Le(100), u64Le(604_900), u64Le(1_209_700),
])).digest();
assert.deepEqual(Buffer.from(await computeDecodedExecutionDigest(program.programId, decoded)), expectedExecution);
assert.equal(await verifyDecodedExecutionDigest(program.programId, {...decoded, executionDigest: expectedExecution}), true);
assert.equal(await verifyDecodedExecutionDigest(program.programId, {...decoded, executionDigest: expectedExecution, executeAfter: new anchor.BN(604_901)}), false);

program.account.governanceControl.fetchNullable = async () => ({manager: keys[7], managerEpoch: new anchor.BN(7)});
const election = await governance.electManager({market: keys[1], proposer: keys[0], manager: keys[7], nonce: 91,
  metadata: verifiedMetadata, contributions: [{owner: keys[0], amount: 1_000}]});
const electionArgs = program.coder.instruction.decode(election.instruction.data).data.args;
assert.deepEqual(electionArgs.updates, []);
assert.equal(electionArgs.action.appointManager.expectedEpoch.toString(), "7");
assert.equal(electionArgs.action.appointManager.manager.toString(), keys[7].toString());
const managerProposal = await governance.proposeAsManager({market: keys[1], manager: keys[7], payer: keys[0], nonce: 92,
  updates: everyFamily.filter(value => value.kind !== "oracle"), metadata});
const managerArgs = program.coder.instruction.decode(managerProposal.instruction.data).data.args;
assert.equal(managerArgs.expectedEpoch.toString(), "7");
assert.equal(managerArgs.policyVersion.toString(), String(MANAGER_POLICY_VERSION));
assert.equal(managerArgs.updates.length, 8);
assert.equal(managerProposal.instruction.keys.filter(key => key.isSigner).length, 2);
assert(managerProposal.instruction.keys.some(key => key.pubkey.equals(deriveGovernanceControlAddress(keys[1], program.programId)[0])));
const managerLookup = new AddressLookupTableAccount({key: keys[8], state: {deactivationSlot: 0xffff_ffff_ffff_ffffn,
  lastExtendedSlot: 0, lastExtendedSlotStartIndex: 0, authority: keys[0],
  addresses: managerProposal.instruction.keys.filter(key => !key.isSigner).map(key => key.pubkey)}});
const managerV0 = parameterProposalV0Transaction({payer: keys[0], recentBlockhash: keys[9].toBase58(),
  proposalInstruction: managerProposal.instruction, lookupTables: [managerLookup]});
assert(managerV0.serialize().length <= SOLANA_TRANSACTION_LIMIT);
await assert.rejects(governance.proposeAsManager({market: keys[1], manager: keys[7], payer: keys[0], nonce: 93,
  updates: [everyFamily.find(value => value.kind === "oracle")], metadata: verifiedMetadata}), /oracle/);

const cancellationTarget = {kind: "cancelProposal", proposal: managerProposal.proposal, digest: managedIntent};
const cancellationPda = (await deriveRecoveryBallotAddress(keys[1], cancellationTarget, program.programId))[0];
assert(cancellationPda.equals(deriveCancellationBallotAddress(keys[1], managerProposal.proposal, program.programId)[0]));
const removalPda = (await deriveRecoveryBallotAddress(keys[1], {kind: "removeManager", epoch: 7}, program.programId))[0];
assert(removalPda.equals(deriveRemovalBallotAddress(keys[1], program.programId)[0]));
assert(removalPda.equals((await deriveRecoveryBallotAddress(keys[1], {kind: "removeManager", epoch: 9}, program.programId))[0]));
assert(!deriveRecoveryVoteAddress(removalPda, 0, balanceAddress, program.programId)[0].equals(deriveRecoveryVoteAddress(removalPda, 1, balanceAddress, program.programId)[0]));
const opened = await governance.openRecoveryBallot({market: keys[1], payer: keys[0], target: cancellationTarget});
assert(opened.ballot.equals(cancellationPda));
program.account.recoveryBallot.fetch = async () => ({target: {cancelProposal: {proposal: managerProposal.proposal, digest: Array.from(managedIntent)}}, round: new anchor.BN(3)});
const recoveryVote = await governance.voteRecovery({market: keys[1], ballot: cancellationPda, source: balanceAddress,
  authority: keys[0], payer: keys[0], amount: 100});
assert(recoveryVote.vote.equals(deriveRecoveryVoteAddress(cancellationPda, 3, balanceAddress, program.programId)[0]));
assert.equal(program.coder.instruction.decode(recoveryVote.instruction.data).data.args.round.toString(), "3");
const releasedVote = await governance.releaseRecoveryVote({market: keys[1], ballot: cancellationPda, source: balanceAddress, round: 2, rentRefund: keys[0]});
assert(releasedVote.instruction.keys.some(key => key.pubkey.equals(deriveRecoveryVoteAddress(cancellationPda, 2, balanceAddress, program.programId)[0])));
for (const [index, status] of ["collecting", "queued", "executed", "cancelled", "expired", "stale"].entries()) assert.equal(decodeProposalOutcome(Uint8Array.of(index)), status);
assert.throws(() => decodeProposalOutcome(Uint8Array.of(6)));
assert.throws(() => decodeProposalOutcome(Uint8Array.of(1, 0)));
for (const [name, value] of [["MANAGER_POLICY_VERSION", MANAGER_POLICY_VERSION], ["MANAGER_PENDING_LIMIT", MANAGER_PENDING_LIMIT],
  ["JUNIOR_MAX_NOTICE_SLOTS", JUNIOR_MAX_NOTICE_SLOTS], ["MANAGER_NOTICE_SLOTS", MANAGER_NOTICE_SLOTS],
  ["MANAGER_EXECUTION_WINDOW_SLOTS", MANAGER_EXECUTION_WINDOW_SLOTS], ["MANAGER_ANNOUNCEMENT_COOLDOWN", MANAGER_ANNOUNCEMENT_COOLDOWN_SECONDS], ["RECOVERY_BALLOT_WINDOW", RECOVERY_BALLOT_WINDOW_SECONDS]]) {
  assert.equal(BigInt(IDL.constants.find(constant => constant.name === name).value), BigInt(value), `SDK ${name} must match the program`);
}
console.log(`Manager and recovery builders: passed; maximum manager v0 ${managerV0.serialize().length}/${SOLANA_TRANSACTION_LIMIT} bytes`);

assert((await governance.getManager(keys[1])).equals(keys[7]));
program.account.governanceControl.fetchNullable = async () => ({manager: PublicKey.default, managerEpoch: new anchor.BN(8)});
assert.equal(await governance.getManager(keys[1]), null);
await assert.rejects(governance.openRemovalBallot({market: keys[1], payer: keys[0]}), /no manager/);
program.account.governanceControl.fetchNullable = async () => null;
assert.equal(await governance.getManager(keys[1]), null);

assert.equal(MANAGER_NOTICE_SLOTS, JUNIOR_MAX_NOTICE_SLOTS + 432_000);
assert.equal(proposalClockUnit({lp: {}}), "unixSeconds");
assert.equal(proposalClockUnit({manager: {}}), "slots");
assert.throws(() => proposalClockUnit({}), /unknown/);
