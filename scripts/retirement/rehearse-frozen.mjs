/** Replay the exact frozen manifest with the actual temporary SBF; no RPC/keys. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { ComputeBudgetProgram, TransactionMessage, VersionedTransaction } from '@solana/web3.js';
import { createAssociatedTokenAccountIdempotentInstruction, TOKEN_PROGRAM_ID } from '@solana/spl-token';
import { LiteSVM, FeatureSet, ComputeBudget, Clock, Rent, FailedTransactionMetadata } from 'litesvm';
import anchor from '@coral-xyz/anchor';
import { readRetirementBundle } from './bundle.mjs';
import { ownerEvidenceTree } from './merkle.mjs';
import { scope, key, bytes, digest, readEvidence, saveEvidence, validateFrozenCapture, LOADER, CLOCK, retirementAddress, freezeAddress, checkedWallets } from './evidence.mjs';
import { compileManifest, beginArgs, retirementInstruction, payoutInstruction } from './manifest.mjs';
import { assertManifestState } from './local.mjs';

export async function rehearseFrozen(capture, bundle, model, quote, proof, manifest) {
  const compiled = await compileManifest(capture, bundle, model, quote, proof);
  assert.equal(digest(compiled), digest(manifest));
  const { rows, idl } = await validateFrozenCapture(capture, bundle), coder = new anchor.BorshCoder(idl);
  const svm = new LiteSVM(), features = new FeatureSet(); let count = 0;
  for (const feature of features.getInactiveFeatures()) {
    const account = svm.getAccount(key(feature));
    if (account?.owner.equals(key('Feature111111111111111111111111111111111111')) && account.data.length === 9 && account.data[0] === 1) {
      features.activate(feature, Buffer.from(account.data).readBigUInt64LE(1)); count++;
    }
  }
  assert.equal(count, 219); features.activate(key('Eoh7e1sDqtyPtuiWAhBNSJinvtJWTTDgeUMRi3RF8zWS').toBytes(), 0n);
  const budget = new ComputeBudget(); budget.computeUnitLimit = 1_400_000n; budget.heapSize = 256 * 1024;
  svm.withFeatureSet(features).withComputeBudget(budget).withBuiltins().withDefaultPrograms().withPrecompiles()
    .withSigverify(false).withBlockhashCheck(false).withTransactionHistory(0n);
  for (const [address, account] of rows) if (account?.executable) {
    let binary = bytes(account);
    if (account.owner === LOADER) binary = bytes(rows.get(key(binary.subarray(4)).toBase58())).subarray(45);
    svm.addProgramWithLoader(key(address), binary, key(account.owner));
  }
  for (const [address, account] of rows) if (account && !account.executable && address !== CLOCK)
    svm.setAccount(key(address), { ...account, owner: key(account.owner), data: bytes(account), rentEpoch: 0 });
  for (const row of checkedWallets(capture.walletObservation, capture.bank)) if (row.account)
    svm.setAccount(key(row.address), { ...row.account, owner: key(row.account.owner), data: Buffer.alloc(0), rentEpoch: 0 });
  const clock = bytes(rows.get(CLOCK)); svm.warpToSlot(BigInt(capture.bank.slot));
  svm.setClock(new Clock(clock.readBigUInt64LE(0), clock.readBigInt64LE(8), clock.readBigUInt64LE(16), clock.readBigUInt64LE(24), clock.readBigInt64LE(32)));
  const rent = bytes(rows.get('SysvarRent111111111111111111111111111111111'));
  svm.setRent(new Rent(rent.readBigUInt64LE(0), rent.readDoubleLE(8), rent[16]));
  const checks = [];
  const send = (label, instructions) => {
    const transaction = new VersionedTransaction(new TransactionMessage({ payerKey: key(scope.authority), recentBlockhash: svm.latestBlockhash(),
      instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.requestHeapFrame({ bytes: 262144 }), ...instructions] }).compileToV0Message());
    assert.ok(transaction.serialize().length <= 1232, `${label}: packet size`);
    const result = svm.sendTransaction(transaction);
    assert.ok(!(result instanceof FailedTransactionMetadata), result instanceof FailedTransactionMetadata ? result.meta().logs().join('\n') : '');
    const events = result.innerInstructions().flatMap((group, nativeGroupIndex) => group.flatMap((inner, innerIndex) => {
      const instruction = inner.instruction(), data = Buffer.from(instruction.data());
      if (!transaction.message.staticAccountKeys[instruction.programIdIndex()].equals(key(scope.program))
        || !data.subarray(0, 8).equals(Buffer.from('e445a52e51cb9a1d', 'hex'))) return [];
      const event = coder.events.decode(data.subarray(8).toString('base64')); assert.ok(event, 'Recoverable typed CPI event required');
      return [{ nativeGroupIndex, innerIndex, stackHeight: inner.stackHeight(), name: event.name, eventData: data.subarray(8).toString('base64') }];
    }));
    assert.equal(events.length, 1, 'Each successful settlement step has one recoverable event');
    checks.push({ label, packetBytes: transaction.serialize().length, computeUnits: String(result.computeUnitsConsumed()), events });
  };
  const tokenBalance = address => {
    const account = svm.getAccount(key(address)); return account ? Buffer.from(account.data).readBigUInt64LE(64) : 0n;
  };
  const before = [...manifest.users, manifest.treasury].map(row => [tokenBalance(row.baseRecipient), tokenBalance(row.quoteRecipient)]);
  send('begin exact frozen manifest', [retirementInstruction(idl, 'begin_legacy_retirement', { args: beginArgs(manifest) }, {},
    [...scope.vaults.map(address => ({ pubkey: key(address), isWritable: true, isSigner: false })),
      ...scope.mints.map(address => ({ pubkey: key(address), isWritable: false, isSigner: false }))])]);
  for (let index = 0; index < scope.owners.length; index++) send(`clear exact owner ${index}`, [retirementInstruction(idl, 'clear_legacy_owner', { owner_index: index, proof: ownerEvidenceTree(manifest.users.map(row => Buffer.from(row.personalSha256, 'hex'))).proofs[index] },
    { owner: scope.owners[index] }, scope.personal[index].map(row => ({ pubkey: key(row.address), isWritable: true, isSigner: false })))]);
  for (let index = 0; index <= scope.owners.length; index++) {
    const row = index === scope.owners.length ? manifest.treasury : manifest.users[index];
    const create = scope.mints.map((mint, side) => createAssociatedTokenAccountIdempotentInstruction(key(scope.authority), key(row[side ? 'quoteRecipient' : 'baseRecipient']), key(row.owner), key(mint), TOKEN_PROGRAM_ID));
    send(index === scope.owners.length ? 'finish exact treasury' : `pay exact owner ${index}`, [...create, payoutInstruction(idl, manifest, index)]);
    assert.equal(String(tokenBalance(row.baseRecipient) - before[index][0]), row.baseRaw);
    assert.equal(String(tokenBalance(row.quoteRecipient) - before[index][1]), row.quoteRaw);
  }
  const state = coder.accounts.decode('LegacyRetirement', Buffer.from(svm.getAccount(key(retirementAddress)).data));
  assertManifestState({ alreadyTemporary: true, idlUploaded: true, idl,
    frozen: { data: Buffer.from(svm.getAccount(key(freezeAddress)).data) }, market: { data: Buffer.from(svm.getAccount(key(scope.market)).data) }, state }, manifest, capture);
  assert.ok(state.completed);
  for (const row of scope.personal.flat()) assert.ok(!svm.getAccount(key(row.address)) || svm.getAccount(key(row.address)).data.length === 0);
  return { schemaVersion: 'dusk-retirement-frozen-execution-rehearsal.v1', localOnly: true, executionReady: false,
    captureSha256: capture.contentSha256, manifestSha256: manifest.contentSha256, onChainManifestSha256: manifest.onChainManifestSha256,
    bundleSha256: digest(bundle.manifest), completed: true, exactPayouts: true, checks,
    limitations: ['VM signature verification disabled only locally', 'Missing destination ATAs are created locally; live submission re-simulates current recipient state'] };
}
const args = process.argv.slice(2);
// Keep imports side-effect free for the synthetic local-bank integration test.
if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  (async () => {
    assert.equal(args.length, 7, 'Expected CAPTURE BUNDLE MODEL_REFERENCE QUOTE MODEL_PROOF MANIFEST NEW_PROOF');
    const [capture, bundlePath, model, quote, proof, manifest, output] = args;
    const result = await rehearseFrozen(readEvidence(capture), readRetirementBundle(bundlePath), readEvidence(model), readEvidence(quote), readEvidence(proof), readEvidence(manifest));
    saveEvidence(output, result); console.log(JSON.stringify({ localOnly: true, completed: true, exactPayouts: true, nativeTransactions: result.checks.length }));
  })().catch(error => { console.error(error.message); process.exitCode = 1; });
}
