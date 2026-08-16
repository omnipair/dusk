import assert from "node:assert/strict";
import { createHash } from "node:crypto";

import anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import {
  anchorParameterUpdate,
  assertProposalTitle,
  assertProposalUri,
  computeParameterProposalDigest,
  createProposalMetadata,
  feeParameterUpdate,
  resolveProposalDescriptionUri,
  tryFetchProposalDescription,
} from "../dist/governance.js";
import { deriveYieldAccountAddress } from "../dist/constants.js";
import IDL from "../dist/idl_v2.js";
import { DuskWrite } from "../dist/write.js";

const SOLANA_TRANSACTION_LIMIT = 1_232;
const EXPECTED_WORST_CASE_CREATE_SIZE = 1_012;
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
  launchFeeStartBps: 500,
  launchFeeDurationSeconds: 3_600,
  launchFeeDecayMode: 1,
  launchRateLimitAsset: 1,
  launchRateLimitReferenceNad: 100_000_000_000n,
  launchRateLimitIncrementBps: 100,
  launchRateLimitMaxFeeBps: 2_000,
  launchRateLimitDurationSeconds: 3_600,
});
assert.equal(launchUpdate.profile.launchFeeStartBps, 500);
assert.equal(launchUpdate.profile.launchRateLimitAsset, 1);
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
const digestNonce = new anchor.BN(7);
const familyRevision = new anchor.BN(11);
const updateBytes = program.coder.types.encode(
  "marketParameterUpdate",
  anchorParameterUpdate(update)
);
const metadataBytes = program.coder.types.encode("proposalMetadataV1", verifiedMetadata);
const u64Le = (value) => value.toArrayLike(Buffer, "le", 8);
const expectedDigest = createHash("sha256")
  .update(
    Buffer.concat([
      Buffer.from("DUSK_PARAMETER_PROPOSAL_V1"),
      program.programId.toBuffer(),
      keys[1].toBuffer(),
      keys[0].toBuffer(),
      u64Le(digestNonce),
      u64Le(familyRevision),
      updateBytes,
      metadataBytes,
    ])
  )
  .digest();
const actualDigest = await computeParameterProposalDigest({
  programId: program.programId,
  market: keys[1],
  proposer: keys[0],
  nonce: digestNonce,
  familyRevision,
  update,
  metadata: verifiedMetadata,
});
assert.deepEqual(Buffer.from(actualDigest), expectedDigest, "proposal digest must match Anchor Borsh");

const build = await new DuskWrite(program).createParameterProposal({
  proposer: keys[0],
  market: keys[1],
  nonce: "18446744073709551615",
  update,
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
  update,
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
