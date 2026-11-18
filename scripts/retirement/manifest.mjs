/** Fixed-amount manifest compiler; no keys, transport, or live submission. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import anchor from '@coral-xyz/anchor';
import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from '@solana/spl-token';
import { readRetirementBundle } from './bundle.mjs';
import { scope, key, bytes, sha, digest, seal, readEvidence, saveEvidence, validateFrozenCapture, freezeAddress, retirementAddress } from './evidence.mjs';

export function ownerEvidence(rows, index) {
  const pieces = [Buffer.from('DUSK_RETIREMENT_PERSONAL_V1'), Buffer.from([index])];
  for (const expected of scope.personal[index]) {
    assert.ok(rows.has(expected.address), 'Personal account omitted from capture');
    const account = rows.get(expected.address), data = account ? bytes(account) : Buffer.alloc(0);
    const owner = account?.owner ?? SystemProgram.programId.toBase58(), length = Buffer.alloc(4);
    assert.ok(!account?.executable && (owner === scope.program && data.subarray(0, 8).equals(Buffer.from(expected.discriminator))
      || owner === SystemProgram.programId.toBase58() && data.length === 0), 'Unexpected personal account');
    length.writeUInt32LE(data.length);
    pieces.push(key(expected.address).toBuffer(), key(owner).toBuffer(), Buffer.from([0]), length, data);
  }
  return sha(Buffer.concat(pieces));
}
export const minimum = value => ((BigInt(value) * 9900n + 9999n) / 10000n).toString();
export function beginArgs(manifest) {
  return { source_slot: new anchor.BN(manifest.sourceSlot), bank_sha256: [...Buffer.from(manifest.frozenBankSha256, 'hex')],
    quote_sha256: [...Buffer.from(manifest.quoteReportSha256, 'hex')],
    owner_evidence_sha256: manifest.users.map(row => [...Buffer.from(row.personalSha256, 'hex')]),
    payouts: manifest.users.map(row => Object.fromEntries([['quoted_base', row.baseRaw], ['quoted_quote', row.quoteRaw], ['base', row.baseRaw], ['quote', row.quoteRaw]]
      .map(([key, value]) => [key, new anchor.BN(value)]))) };
}
export function retirementInstruction(idl, name, args = {}, overrides = {}, remaining = []) {
  const definition = idl.instructions.find(row => row.name === name); assert.ok(definition);
  const defaults = { authority: scope.authority, program_data: scope.programData, frozen: freezeAddress, market: scope.market,
    retirement: retirementAddress, system_program: SystemProgram.programId, token_program: TOKEN_PROGRAM_ID,
    base_vault: scope.vaults[0], quote_vault: scope.vaults[4], base_mint: scope.mints[0], quote_mint: scope.mints[1],
    event_authority: PublicKey.findProgramAddressSync([Buffer.from('__event_authority')], key(scope.program))[0], program: scope.program };
  return new TransactionInstruction({ programId: key(scope.program), data: new anchor.BorshCoder(idl).instruction.encode(name, args),
    keys: [...definition.accounts.map(row => ({ pubkey: key(overrides[row.name] ?? defaults[row.name] ?? row.address),
      isSigner: !!row.signer, isWritable: !!row.writable })), ...remaining] });
}
export function payoutInstruction(idl, manifest, index) {
  const treasury = index === scope.owners.length, row = treasury ? manifest.treasury : manifest.users[index];
  assert.ok(row);
  return retirementInstruction(idl, treasury ? 'finish_legacy_retirement' : 'pay_legacy_owner', treasury ? {} : { owner_index: index },
    { base_recipient: row.baseRecipient, quote_recipient: row.quoteRecipient });
}
export async function compileManifest(capture, bundle, model, quote, proof) {
  const { rows, frozen, cutoffSlot, idl } = await validateFrozenCapture(capture, bundle);
  assert.equal(model.bankKind, 'reconstructed-quote-model'); assert.equal(model.bankSha256, digest(model.bank));
  assert.equal(model.sourceModel.actualFrozenCaptureSha256, capture.contentSha256);
  assert.equal(model.sourceModel.actualBankSha256, capture.bankSha256);
  assert.equal(model.sourceModel.temporaryBundleSha256, digest(bundle.manifest));
  assert.equal(model.sourceModel.cutoffSlot, cutoffSlot);
  assert.equal(quote.inventorySha256, model.inventorySha256);
  assert.equal(quote.proposal.status, 'proposed-not-authorized-for-execution');
  assert.equal(quote.schemaVersion, 'dusk-legacy-close-rehearsal.v1');
  assert.equal(quote.bankSha256, digest(quote.bank));
  const modeledRows = new Map(model.bank.accounts.map(row => [row.address, row.account]));
  for (const row of quote.bank.accounts) assert.equal(digest(row.account), digest(modeledRows.get(row.address)), 'Quote differs from cutoff model');
  assert.equal(proof.schemaVersion, 'dusk-retirement-program-rehearsal.v1');
  assert.equal(proof.sourceBankSha256, model.bankSha256); assert.equal(proof.quoteReportSha256, quote.contentSha256);
  assert.equal(proof.temporaryBinarySha256, bundle.manifest.files['dusk.so'].sha256);
  assert.equal(proof.temporaryIdlSha256, digest(idl)); assert.equal(proof.localOnly, true);
  const policy = quote.proposal.policy;
  assert.equal(policy.maximumNetPayoutSlippageBps, 100); assert.equal(policy.juniorExitFeeBps, 50);
  assert.equal(policy.immediateJuniorTerminalSettlement, true); assert.equal(policy.treasuryTopUp, false);
  assert.equal(quote.proposal.treasury.owner, scope.treasury);
  assert.deepEqual(quote.proposal.users.map(row => row.owner).sort(), [...scope.owners].sort());
  const recipients = owner => Object.fromEntries(scope.mints.map((mint, index) => [index ? 'quoteRecipient' : 'baseRecipient',
    getAssociatedTokenAddressSync(key(mint), key(owner), true, TOKEN_PROGRAM_ID).toBase58()]));
  const users = scope.owners.map((owner, index) => {
    const row = quote.proposal.users.find(row => row.owner === owner), actual = proof.userReceipts.find(row => row.owner === owner);
    assert.ok(row && actual);
    for (const side of ['base', 'quote']) {
      assert.match(row[`${side}Raw`], /^(0|[1-9][0-9]*)$/); assert.ok(BigInt(row[`${side}Raw`]) < 1n << 64n);
      assert.equal(row[`${side}Raw`], actual[`${side}Raw`]);
    }
    return { owner, baseRaw: row.baseRaw, quoteRaw: row.quoteRaw, minimumBaseRaw: minimum(row.baseRaw), minimumQuoteRaw: minimum(row.quoteRaw),
      personalSha256: ownerEvidence(rows, index), ...recipients(owner) };
  });
  const custody = [0, 1].map(side => frozen.vault_amounts.slice(side * 4, side * 4 + 4).reduce((sum, value) => sum + BigInt(value.toString()), 0n));
  const treasury = { owner: scope.treasury, ...recipients(scope.treasury) };
  for (const [index, side] of ['base', 'quote'].entries()) {
    const paid = users.reduce((sum, row) => sum + BigInt(row[`${side}Raw`]), 0n); assert.ok(paid <= custody[index]);
    treasury[`${side}Raw`] = String(custody[index] - paid);
    assert.equal(treasury[`${side}Raw`], quote.proposal.treasury[`${side}Raw`]);
    assert.equal(treasury[`${side}Raw`], proof.treasuryReceipts[`${side}Raw`]);
  }
  const manifest = { schemaVersion: 'dusk-retirement-payout-manifest.v1', localOnly: capture.localOnly, executionAuthorized: false,
    sourceSlot: capture.bank.slot, cutoffSlot, market: scope.market, program: scope.program, authority: scope.authority,
    frozenCaptureSha256: capture.contentSha256, frozenBankSha256: capture.bankSha256, modelReferenceSha256: model.contentSha256,
    quoteReportSha256: quote.contentSha256, nativeProofSha256: proof.contentSha256, bundleSha256: digest(bundle.manifest),
    users, treasury, custodyRaw: custody.map(String), preservedOwnerOrderRentClaim: scope.preservedOwnerOrderRentClaim,
    notes: ['Final net OMFG/META; no external top-up', 'Immutable funding attestation; not an on-chain proof of curve economics',
      'Owner receipt-token cleanup and ordinary empty-order cancellation remain owner actions'] };
  manifest.onChainManifestSha256 = sha(new anchor.BorshCoder(idl).instruction.encode('begin_legacy_retirement', { args: beginArgs(manifest) }).subarray(8));
  return seal(manifest);
}
async function main() {
  const [capture, bundle, model, quote, proof, output] = process.argv.slice(2);
  assert.equal(process.argv.length, 8, 'Expected CAPTURE BUNDLE MODEL_REFERENCE QUOTE NATIVE_PROOF NEW_MANIFEST');
  const manifest = await compileManifest(readEvidence(capture), readRetirementBundle(bundle), readEvidence(model), readEvidence(quote), readEvidence(proof));
  const { contentSha256, ...body } = manifest; saveEvidence(output, body);
  console.log(JSON.stringify({ executionAuthorized: false, localOnly: manifest.localOnly, users: manifest.users.map(({ owner, baseRaw, quoteRaw }) => ({ owner, baseRaw, quoteRaw })), treasury: manifest.treasury }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
