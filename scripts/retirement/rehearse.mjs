/** Local-only SBF lifecycle rehearsal. No signing keys, network or RPC client. */
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import anchor from '@coral-xyz/anchor';
import { PublicKey, SystemProgram, TransactionInstruction, TransactionMessage, VersionedTransaction, ComputeBudgetProgram } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, createInitializeAccount3Instruction, createApproveInstruction, createRevokeInstruction, createTransferCheckedInstruction, createBurnCheckedInstruction } from '@solana/spl-token';
import { LiteSVM, FeatureSet, ComputeBudget, Clock, Rent, FailedTransactionMetadata } from 'litesvm';
import { seal, digest } from './evidence.mjs';

const directory = fileURLToPath(new URL('.', import.meta.url));
const { BorshCoder, BN } = anchor;
const scope = JSON.parse(readFileSync(resolve(directory, 'scope.json')));
const key = value => new PublicKey(value);
const hash = data => createHash('sha256').update(data).digest();
const canonical = value => value === null || typeof value !== 'object' ? JSON.stringify(value)
  : Array.isArray(value) ? `[${value.map(canonical).join(',')}]`
    : `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(',')}}`;
function evidence(file) {
  const { contentSha256, ...body } = JSON.parse(readFileSync(file));
  assert.equal(hash(canonical(body)).toString('hex'), contentSha256, 'Evidence hash differs');
  return { ...body, contentSha256 };
}

const [sourceFile, quoteFile, binaryFile, idlFile, outputFile, successorBinaryFile, successorIdlFile, delegateBinaryFile, delegateIdlFile] = process.argv.slice(2);
assert.ok([5, 9].includes(process.argv.length - 2), 'Supply either five inputs or four additional successor artifacts');
assert.ok(sourceFile && quoteFile && binaryFile && idlFile && outputFile, 'Expected SOURCE_BANK_REPORT QUOTE_REPORT RETIREMENT_SBF RETIREMENT_IDL NEW_OUTPUT');
const source = evidence(sourceFile), quote = evidence(quoteFile);
assert.equal(source.schemaVersion, 'dusk-legacy-close-rehearsal.v1');
assert.equal(quote.schemaVersion, 'dusk-legacy-close-rehearsal.v1');
assert.equal(quote.proposal.status, 'proposed-not-authorized-for-execution');
assert.equal(quote.proposal.users.length, 5);
assert.deepEqual(quote.proposal.users.map(row => row.owner).sort(), [...scope.owners].sort());
assert.equal(quote.proposal.policy.maximumNetPayoutSlippageBps, 100);
assert.equal(quote.proposal.policy.juniorExitFeeBps, 50);
assert.equal(quote.proposal.policy.immediateJuniorTerminalSettlement, true);
assert.equal(quote.proposal.policy.treasuryTopUp, false);
assert.equal(quote.proposal.treasury.owner, scope.treasury);
assert.equal(source.bank.slot, quote.bank.slot, 'Quote is from a different bank');
assert.equal(hash(canonical(source.bank)).toString('hex'), source.bankSha256);
const rows = new Map(source.bank.accounts.map(row => [row.address, row.account]));
for (const expected of [scope.legacyIdentity.dusk, scope.legacyIdentity.delegate]) {
  const account = rows.get(expected.programId), metadata = rows.get(expected.deployment.programData);
  assert.equal(account.owner, 'BPFLoaderUpgradeab1e11111111111111111111111');
  assert.equal(metadata.owner, account.owner); assert.ok(account.executable && !metadata.executable);
  const data = Buffer.from(metadata.data, 'base64');
  assert.equal(data.length, expected.deployment.allocatedBinaryBytes + 45);
  assert.equal(data.readBigUInt64LE(4), BigInt(expected.deployment.deploySlot));
  assert.equal(data[12], 1); assert.equal(key(data.subarray(13, 45)).toBase58(), scope.authority);
  assert.equal(hash(data.subarray(45)).toString('hex'), expected.binarySha256);
}
for (const row of quote.bank.accounts) assert.equal(canonical(row.account), canonical(rows.get(row.address)), 'Quote bank is not the exact captured subset');
const binary = readFileSync(binaryFile), idl = JSON.parse(readFileSync(idlFile)), coder = new BorshCoder(idl);
assert.equal(idl.address, scope.program);
assert.deepEqual(idl.instructions.map(row => row.name).sort(), ['abort_stale_legacy_freeze', 'begin_legacy_retirement', 'clear_legacy_owner', 'finish_legacy_retirement', 'freeze_legacy_market', 'pay_legacy_owner']);
const program = key(scope.program), authority = key(scope.authority), market = key(scope.market);
const loader = key('BPFLoaderUpgradeab1e11111111111111111111111');
const eventAuthority = PublicKey.findProgramAddressSync([Buffer.from('__event_authority')], program)[0];
const state = PublicKey.findProgramAddressSync([Buffer.from('legacy_retirement'), market.toBuffer()], program)[0];
const frozen = PublicKey.findProgramAddressSync([Buffer.from('legacy_freeze'), market.toBuffer()], program)[0];
const sourceMarket = rows.get(scope.market);
assert.equal(sourceMarket.owner, scope.program);
assert.ok(sourceMarket.data);
const ownerDigests = scope.personal.map((accounts, index) => {
  const digest = createHash('sha256').update('DUSK_RETIREMENT_PERSONAL_V1').update(Buffer.from([index]));
  for (const expected of accounts) {
    assert.ok(rows.has(expected.address), 'Personal account omitted from atomic bank');
    const account = rows.get(expected.address), data = account ? Buffer.from(account.data, 'base64') : Buffer.alloc(0);
    const owner = account?.owner ?? SystemProgram.programId.toBase58(), length = Buffer.alloc(4);
    assert.ok(owner === scope.program && data.subarray(0, 8).equals(Buffer.from(expected.discriminator))
      || owner === SystemProgram.programId.toBase58() && data.length === 0, 'Unexpected personal account');
    length.writeUInt32LE(data.length);
    digest.update(key(expected.address).toBuffer()).update(key(owner).toBuffer())
      .update(Buffer.from([Number(account?.executable ?? false)])).update(length).update(data);
  }
  return [...digest.digest()];
});
const args = {
  source_slot: new BN(source.bank.slot),
  bank_sha256: [...Buffer.from(source.bankSha256, 'hex')], quote_sha256: [...Buffer.from(quote.contentSha256, 'hex')],
  owner_evidence_sha256: ownerDigests,
  payouts: scope.owners.map(owner => {
    const row = quote.proposal.users.find(user => user.owner === owner); assert.ok(row, 'Missing payout owner');
    return { quoted_base: new BN(row.baseRaw), quoted_quote: new BN(row.quoteRaw), base: new BN(row.baseRaw), quote: new BN(row.quoteRaw) };
  }),
};

function runtime() {
  const svm = new LiteSVM(), features = new FeatureSet();
  let count = 0;
  for (const feature of features.getInactiveFeatures()) {
    const account = svm.getAccount(new PublicKey(feature));
    if (account?.owner.equals(key('Feature111111111111111111111111111111111111')) && account.data.length === 9 && account.data[0] === 1) {
      features.activate(feature, Buffer.from(account.data).readBigUInt64LE(1)); count++;
    }
  }
  features.activate(key('Eoh7e1sDqtyPtuiWAhBNSJinvtJWTTDgeUMRi3RF8zWS').toBytes(), 0n);
  assert.equal(count, 219); assert.equal(features.getActiveFeaturesCount(), 220);
  const budget = new ComputeBudget(); budget.computeUnitLimit = 1_400_000n; budget.heapSize = 256 * 1024;
  svm.withFeatureSet(features).withComputeBudget(budget).withBuiltins().withDefaultPrograms().withPrecompiles()
    .withSigverify(false).withBlockhashCheck(false).withTransactionHistory(0n);
  for (const row of source.bank.accounts) {
    const account = row.account;
    if (!account?.executable || ['11111111111111111111111111111111', 'ComputeBudget111111111111111111111111111111'].includes(row.address)) continue;
    let bytes = Buffer.from(account.data, 'base64');
    if (account.owner === loader.toBase58()) bytes = Buffer.from(rows.get(new PublicKey(bytes.subarray(4)).toBase58()).data, 'base64').subarray(45);
    svm.addProgramWithLoader(key(row.address), row.address === scope.program ? binary : bytes, key(account.owner));
  }
  for (const row of source.bank.accounts) {
    const account = row.account;
    if (!account || account.executable || row.address === 'SysvarC1ock11111111111111111111111111111111') continue;
    assert.ok(Number.isSafeInteger(account.lamports));
    svm.setAccount(key(row.address), { ...account, owner: key(account.owner), data: Buffer.from(account.data, 'base64'), rentEpoch: 0 });
  }
  const rawClock = Buffer.from(rows.get('SysvarC1ock11111111111111111111111111111111').data, 'base64');
  svm.warpToSlot(BigInt(source.bank.slot));
  svm.setClock(new Clock(rawClock.readBigUInt64LE(0), rawClock.readBigInt64LE(8), rawClock.readBigUInt64LE(16), rawClock.readBigUInt64LE(24), rawClock.readBigInt64LE(32)));
  const rent = Buffer.from(rows.get('SysvarRent111111111111111111111111111111111').data, 'base64');
  svm.setRent(new Rent(rent.readBigUInt64LE(0), rent.readDoubleLE(8), rent[16]));
  // Emulate only the local code upgrade and its loader metadata. Economic
  // accounts and token amounts remain exactly the captured source bytes.
  installLocalCode(svm, binary);
  return svm;
}

function installLocalCode(svm, bytes) {
  svm.addProgramWithLoader(program, bytes, loader);
  const dataRow = rows.get(scope.programData), oldData = Buffer.from(dataRow.data, 'base64');
  assert.ok(bytes.length <= oldData.length - 45);
  const upgraded = Buffer.alloc(oldData.length); oldData.copy(upgraded, 0, 0, 45);
  upgraded.writeBigUInt64LE(BigInt(source.bank.slot), 4); bytes.copy(upgraded, 45);
  svm.setAccount(key(scope.programData), { ...dataRow, owner: loader, data: upgraded, rentEpoch: 0 });
}

function instruction(name, values, accounts, extra = []) {
  const definition = idl.instructions.find(row => row.name === name); assert.ok(definition);
  return new TransactionInstruction({ programId: program, data: coder.instruction.encode(name, values),
    keys: [...definition.accounts.map(row => ({ pubkey: key(accounts[row.name] ?? row.address ?? ({ event_authority: eventAuthority, program })[row.name]), isSigner: !!row.signer, isWritable: !!row.writable })), ...extra] });
}
const beginAccounts = { retirement: state, frozen, authority, market, program_data: key(scope.programData), system_program: SystemProgram.programId, token_program: TOKEN_PROGRAM_ID };
const extra = [...scope.vaults.map(address => ({ pubkey: key(address), isSigner: false, isWritable: true })),
  ...scope.mints.map(address => ({ pubkey: key(address), isSigner: false, isWritable: false }))];
const begin = (values = args, accounts = beginAccounts, remaining = extra) => instruction('begin_legacy_retirement', { args: values }, accounts, remaining);
const freezeAccounts = { authority, market, frozen, program_data: key(scope.programData), system_program: SystemProgram.programId };
const freezeExtra = [...scope.receiptMints, ...scope.receiptAccounts, ...scope.vaults].map(address => ({ pubkey: key(address), isSigner: false, isWritable: false }));
const freezeInstruction = (accounts = freezeAccounts, remaining = freezeExtra) => instruction('freeze_legacy_market', {}, accounts, remaining);
const checks = [], transactions = [];
function send(svm, instructions, label, reject = false, payer = authority) {
  const tx = new VersionedTransaction(new TransactionMessage({ payerKey: payer, recentBlockhash: svm.latestBlockhash(), instructions: [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...instructions,
  ] }).compileToV0Message());
  assert.ok(tx.serialize().length <= 1232, `${label} packet limit (${tx.serialize().length})`);
  const result = svm.sendTransaction(tx), failed = result instanceof FailedTransactionMetadata;
  if (reject) {
    assert.ok(failed, `${label} unexpectedly succeeded`);
    const logs = result.meta().logs();
    if (typeof reject === 'string') assert.ok(logs.some(row => row.includes(`Error Code: ${reject}.`)), `${label} failed for another reason: ${logs.join('\n')}`);
    checks.push({ label, rejected: true, error: result.err().toString(), logs }); return result;
  }
  assert.ok(!failed, `${label}: ${failed ? result.err().toString() + '\n' + result.meta().logs().join('\n') : ''}`);
  transactions.push({ label, packetBytes: tx.serialize().length, computeUnits: String(result.computeUnitsConsumed()), unsignedTransaction: Buffer.from(tx.serialize()).toString('base64') });
  return result;
}
const amount = (svm, address) => { const account = svm.getAccount(key(address)); return account ? Buffer.from(account.data).readBigUInt64LE(64) : 0n; };
const readState = svm => coder.accounts.decode('LegacyRetirement', Buffer.from(svm.getAccount(state).data));
let svm = runtime();
const beforeMarket = Buffer.from(svm.getAccount(market).data);
send(svm, [freezeInstruction({ ...freezeAccounts, authority: key(scope.owners[4]) })], 'unauthorized freeze', 'ConstraintAddress', key(scope.owners[4]));
send(svm, [freezeInstruction(freezeAccounts, freezeExtra.slice(1))], 'freeze cannot omit a receipt mint or holder', 'Scope');
assert.equal(svm.getAccount(frozen), null);
send(svm, [freezeInstruction()], 'freeze exact receipt supply and settlement clock');
send(svm, [freezeInstruction()], 'repeated freeze', true);
const frozenState = coder.accounts.decode('FrozenLegacyMarket', Buffer.from(svm.getAccount(frozen).data));
assert.deepEqual(Buffer.from(frozenState.clock_bytes), Buffer.from(rows.get('SysvarC1ock11111111111111111111111111111111').data, 'base64'));
assert.deepEqual(Buffer.from(frozenState.market_sha256), hash(beforeMarket));
const snapshot = Buffer.from(frozenState.receipt_snapshot);
let cursor = 4;
assert.equal(snapshot.readUInt32LE(0), scope.receiptMints.length + scope.receiptAccounts.length);
for (const expected of freezeExtra.slice(0, scope.receiptMints.length + scope.receiptAccounts.length)) {
  const address = key(snapshot.subarray(cursor, cursor + 32)).toBase58(); cursor += 32;
  const tokenProgram = key(snapshot.subarray(cursor, cursor + 32)).toBase58(); cursor += 32;
  const length = snapshot.readUInt32LE(cursor); cursor += 4;
  assert.equal(address, expected.pubkey.toBase58()); assert.equal(tokenProgram, rows.get(address).owner);
  assert.deepEqual(snapshot.subarray(cursor, cursor + length), Buffer.from(rows.get(address).data, 'base64')); cursor += length;
}
assert.equal(cursor, snapshot.length);
assert.deepEqual(frozenState.vault_amounts.map(String), scope.vaults.map(address => String(Buffer.from(rows.get(address).data, 'base64').readBigUInt64LE(64))));
// Optional fixture for testing the offline compiler against an actual native
// freeze. It is explicitly a local VM witness, never an observed RPC capture.
if (process.env.DUSK_RETIREMENT_LOCAL_FREEZE_WITNESS) {
  const blueprint = evidence(process.env.DUSK_RETIREMENT_LOCAL_BLUEPRINT);
  const manifest = JSON.parse(readFileSync(process.env.DUSK_RETIREMENT_LOCAL_BUNDLE_MANIFEST));
  assert.equal(manifest.files['dusk.so'].sha256, hash(binary).toString('hex'));
  const addresses = [...source.bank.accounts.map(row => row.address), frozen.toBase58()];
  const raw = address => {
    const account = svm.getAccount(key(address));
    return account ? { owner: account.owner.toBase58(), executable: account.executable,
      lamports: Number(account.lamports), rentEpoch: '0', data: Buffer.from(account.data).toString('base64') } : null;
  };
  const bank = { slot: source.bank.slot, accounts: addresses.map(address => ({ address, account: raw(address) })) };
  const programCatalogs = blueprint.programs.map(catalog => ({ program: catalog.program, context: { slot: source.bank.slot },
    value: catalog.value.map(row => {
      const account = rows.has(row.pubkey) ? raw(row.pubkey) : { ...row.account, rentEpoch: '0', data: row.account.data[0] };
      return { pubkey: row.pubkey, account: { ...account, data: [account.data, 'base64'] } };
    }) }));
  programCatalogs[0].value.push({ pubkey: frozen.toBase58(), account: { ...raw(frozen), data: [raw(frozen).data, 'base64'] } });
  const witness = seal({ schemaVersion: 'dusk-retirement-frozen-capture.v1', localOnly: true, readOnly: false, executionReady: false,
    genesisHash: scope.legacyIdentity.genesisHash, bundleSha256: digest(manifest), bankSha256: digest(bank), bank, programCatalogs,
    provenance: 'Native VM freeze over a captured bank; uncaptured ancillary metadata is an older local fixture. Not RPC-observed state.' });
  writeFileSync(process.env.DUSK_RETIREMENT_LOCAL_FREEZE_WITNESS, JSON.stringify(witness, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
}
const burnable = scope.receiptAccounts.find(address => {
  const data = Buffer.from(rows.get(address).data, 'base64');
  return scope.owners.includes(key(data.subarray(32, 64)).toBase58()) && data.readBigUInt64LE(64) > 0n;
});
assert.ok(burnable);
const burnData = Buffer.from(rows.get(burnable).data, 'base64'), burnMint = key(burnData.subarray(0, 32)), burnOwner = key(burnData.subarray(32, 64));
assert.ok(PublicKey.isOnCurve(burnOwner.toBytes()));
send(svm, [createBurnCheckedInstruction(key(burnable), burnMint, burnOwner, 1n, Buffer.from(rows.get(burnMint.toBase58()).data, 'base64')[44], [], TOKEN_2022_PROGRAM_ID)], 'owner burns obsolete receipt after cutoff without losing frozen entitlement');
assert.deepEqual(Buffer.from(coder.accounts.decode('FrozenLegacyMarket', Buffer.from(svm.getAccount(frozen).data)).receipt_snapshot), snapshot);
checks.push({label:'post-freeze owner receipt burn preserves the recorded entitlement',passed:true});
send(svm, [instruction('abort_stale_legacy_freeze', {}, { authority, market, frozen, retirement: state })], 'review cannot reset an unchanged interest cutoff', 'Evidence');
const delayed = svm.getClock(); delayed.slot += 10_000n; delayed.unixTimestamp += 4000n;
svm.warpToSlot(delayed.slot); svm.setClock(delayed);
checks.push({ label: 'review delay leaves frozen receipt bytes and interest cutoff unchanged', passed: true });
for (const [label, tx, expected, payer] of [
  ['foreign begin authority', begin(args, { ...beginAccounts, authority: key(scope.owners[4]) }), 'ConstraintAddress', key(scope.owners[4])],
  ['missing bank evidence', begin({ ...args, bank_sha256: Array(32).fill(0) }), 'Evidence'],
  ['omitted custody account', begin(args, beginAccounts, extra.slice(1)), 'Scope'],
  ['oversubscribed custody', begin({ ...args, payouts: args.payouts.map((p, i) => i === 0 ? { ...p, base: new BN('18446744073709551615') } : p) }), 'Math'],
  ['below reviewed minimum', begin({ ...args, payouts: args.payouts.map((p, i) => i === 0 ? { ...p, base: new BN(0) } : p) }), 'Minimum'],
]) {
  send(svm, [tx], label, expected, payer);
  assert.ok(Buffer.from(svm.getAccount(market).data).equals(beforeMarket)); assert.equal(svm.getAccount(state), null);
}
send(svm, [begin()], 'begin fixed funded retirement');
send(svm, [begin()], 'repeated begin', true);
assert.notDeepEqual([...svm.getAccount(market).data.subarray(0, 8)], [...beforeMarket.subarray(0, 8)]);
// A code rollback must not make old receipts/positions redeemable again.
const originalProgramData = Buffer.from(rows.get(scope.programData).data, 'base64');
installLocalCode(svm, originalProgramData.subarray(45));
const oldClose = quote.runs.find(row => row.strategy === quote.proposal.strategy).steps.find(row => row.status === 'closed-locally');
const rollback = svm.sendTransaction(VersionedTransaction.deserialize(Buffer.from(oldClose.unsignedTransaction, 'base64')));
assert.ok(rollback instanceof FailedTransactionMetadata);
assert.ok(rollback.meta().logs().some(row => row.includes('AccountDiscriminatorMismatch')), rollback.meta().logs().join('\n'));
checks.push({ label: 'old-binary rollback cannot redeem tombstoned market', rejected: true, logs: rollback.meta().logs() });
installLocalCode(svm, binary);
const destinations = [];
for (let index = 0; index <= 5; index++) {
  const owner = key(index === 5 ? scope.treasury : scope.owners[index]), pair = [];
  for (let side = 0; side < 2; side++) {
    const seed = `retirement-${index}-${side}`, destination = await PublicKey.createWithSeed(authority, seed, TOKEN_PROGRAM_ID);
    send(svm, [SystemProgram.createAccountWithSeed({ fromPubkey: authority, basePubkey: authority, seed, newAccountPubkey: destination,
      lamports: Number(svm.minimumBalanceForRentExemption(165n)), space: 165, programId: TOKEN_PROGRAM_ID }),
      createInitializeAccount3Instruction(destination, key(scope.mints[side]), owner)], `prepare ${index}/${side} owner account`);
    pair.push(destination);
  }
  destinations.push(pair);
}
function payout(index, recipient = destinations[index]) {
  return instruction(index === 5 ? 'finish_legacy_retirement' : 'pay_legacy_owner', index === 5 ? {} : { owner_index: index }, {
    retirement: state, market, base_vault: key(scope.vaults[0]), quote_vault: key(scope.vaults[4]),
    base_mint: key(scope.mints[0]), quote_mint: key(scope.mints[1]), base_recipient: recipient[0], quote_recipient: recipient[1], token_program: TOKEN_PROGRAM_ID,
  });
}
send(svm, [payout(0)], 'payout before all debt and claim accounts cleared', 'Unsettled');
send(svm, [payout(5)], 'treasury before user settlement', 'Unsettled');
const rentReceipts = [];
for (let index = 0; index < 5; index++) {
  const owner = key(scope.owners[index]), before = svm.getBalance(owner) ?? 0n;
  const accounts = scope.personal[index].map(row => ({ pubkey: key(row.address), isSigner: false, isWritable: true }));
  const context = { retirement: state, market, owner };
  if (index === 2) {
    send(svm, [instruction('clear_legacy_owner', { owner_index: index }, context, accounts.slice(1))], 'omitted personal claim', 'Scope');
    assert.equal(readState(svm).cleaned_mask, 3);
  }
  send(svm, [instruction('clear_legacy_owner', { owner_index: index }, context, accounts)], `clear ${index} personal claims and return rent`);
  const returned = svm.getBalance(owner) - before;
  assert.equal(returned, scope.personal[index].reduce((total, row) => total + BigInt(rows.get(row.address)?.lamports ?? 0), 0n));
  rentReceipts.push({ owner: scope.owners[index], returnedLamports: String(returned) });
  send(svm, [instruction('clear_legacy_owner', { owner_index: index }, context, accounts)], `repeated cleanup ${index}`, 'Repeated');
  if (index === 2) {
    // Import only the committed VM state into a fresh process-equivalent VM.
    // This tests restart/resumption; it does not modify source-bank economics.
    const reopened = runtime();
    const addresses = new Set([...rows.keys(), state.toBase58(), frozen.toBase58(), ...destinations.flat().map(row => row.toBase58())]);
    for (const address of addresses) {
      if (address === scope.program || address === scope.programData) continue;
      const account = svm.getAccount(key(address));
      reopened.setAccount(key(address), account ?? { owner: SystemProgram.programId, data: Buffer.alloc(0), lamports: 0, executable: false, rentEpoch: 0 });
    }
    reopened.warpToSlot(svm.getClock().slot); reopened.setClock(svm.getClock()); svm = reopened;
    assert.equal(readState(svm).cleaned_mask, 7); assert.equal(readState(svm).paid_mask, 0);
    checks.push({ label: 'interrupted cleanup resumes from committed evidence with masks intact', passed: true });
  }
}
assert.equal(readState(svm).cleaned_mask, 31);
send(svm, [payout(0, destinations[1])], 'payout redirected to another owner', 'Recipient');
send(svm, [createApproveInstruction(destinations[0][0], authority, key(scope.owners[0]), 1n)], 'owner-approved unsafe recipient setup');
send(svm, [payout(0)], 'recipient has spending delegate', 'Recipient');
assert.equal(readState(svm).paid_mask, 0);
send(svm, [createRevokeInstruction(destinations[0][0], key(scope.owners[0]))], 'remove recipient delegate and retry safely');
const userReceipts = [];
for (let index = 0; index < 5; index++) {
  if (index === 2) {
    const reserve = amount(svm, scope.vaults[0]);
    send(svm, [payout(index, [destinations[index][0], destinations[3][1]])], 'second transfer failure rolls back first asset and paid flag', 'Recipient');
    assert.equal(amount(svm, destinations[index][0]), 0n);
    assert.equal(amount(svm, scope.vaults[0]), reserve); assert.equal(readState(svm).paid_mask, 3);
  }
  send(svm, [payout(index)], `pay immutable owner ${index}`);
  const actual = destinations[index].map(address => String(amount(svm, address)));
  assert.deepEqual(actual, [args.payouts[index].base.toString(), args.payouts[index].quote.toString()]);
  userReceipts.push({ owner: scope.owners[index], baseRaw: actual[0], quoteRaw: actual[1] });
  send(svm, [payout(index)], `repeated payout ${index}`, 'Repeated');
  if (index < 4) send(svm, [payout(5)], `early treasury after owner ${index}`, 'Unsettled');
}
send(svm, [payout(5)], 'pay approved treasury residual and finalize');
send(svm, [payout(5)], 'repeated treasury finalization', 'Repeated');
assert.ok(readState(svm).completed); assert.deepEqual(readState(svm).remaining.map(String), ['0', '0']);
const treasuryReceipts = destinations[5].map(address => String(amount(svm, address)));
assert.deepEqual(treasuryReceipts, [quote.proposal.treasury.baseRaw, quote.proposal.treasury.quoteRaw]);
assert.ok(scope.vaults.every(address => amount(svm, address) === 0n));
assert.ok(scope.personal.flat().every(row => !svm.getAccount(key(row.address)) || svm.getAccount(key(row.address)).data.length === 0));
const primaryTransactions = [...transactions], donated = runtime();
const donorRow = source.bank.accounts.find(row => {
  if (row.account?.owner !== TOKEN_PROGRAM_ID.toBase58()) return false;
  const data = Buffer.from(row.account.data, 'base64');
  return data.length === 165 && key(data.subarray(0, 32)).toBase58() === scope.mints[0]
    && scope.owners.includes(key(data.subarray(32, 64)).toBase58()) && data.readBigUInt64LE(64) > 0n;
});
assert.ok(donorRow);
for (const tx of primaryTransactions) {
  const result = donated.sendTransaction(VersionedTransaction.deserialize(Buffer.from(tx.unsignedTransaction, 'base64')));
  assert.ok(!(result instanceof FailedTransactionMetadata), result instanceof FailedTransactionMetadata ? result.meta().logs().join('\n') : '');
  if (tx.label === 'freeze exact receipt supply and settlement clock') {
    const donor = key(Buffer.from(donorRow.account.data, 'base64').subarray(32, 64));
    send(donated, [createTransferCheckedInstruction(key(donorRow.address), key(scope.mints[0]), key(scope.vaults[3]), donor, 1n, 6)], 'isolated variant: post-cutoff donation');
  }
}
assert.equal(amount(donated, scope.vaults[3]), 1n);
assert.deepEqual(destinations[5].map(address => String(amount(donated, address))), treasuryReceipts);
assert.ok(readState(donated).completed);
checks.push({label:'post-cutoff donation cannot change user payouts or approved treasury residue',passed:true});
let successorCompatibility;
if (successorBinaryFile) {
  const successorBinary = readFileSync(successorBinaryFile), successorIdl = JSON.parse(readFileSync(successorIdlFile));
  const successorCoder = new BorshCoder(successorIdl);
  installLocalCode(svm, successorBinary);
  const cannotReopen = svm.sendTransaction(VersionedTransaction.deserialize(Buffer.from(oldClose.unsignedTransaction, 'base64')));
  assert.ok(cannotReopen instanceof FailedTransactionMetadata);
  assert.ok(cannotReopen.meta().logs().some(row => row.includes('AccountDiscriminatorMismatch')));
  const fTag = Buffer.from(successorIdl.accounts.find(row => row.name === 'FutarchyAuthority').discriminator);
  const fRow = source.bank.accounts.find(row => row.account?.owner === scope.program && Buffer.from(row.account.data, 'base64').subarray(0, 8).equals(fTag));
  assert.ok(fRow); const before = Buffer.from(svm.getAccount(key(fRow.address)).data);
  successorCoder.accounts.decode('FutarchyAuthority', before);
  const eventAuthority = PublicKey.findProgramAddressSync([Buffer.from('__event_authority')], program)[0];
  send(svm, [new TransactionInstruction({ programId: program, keys: [
    { pubkey: authority, isWritable: true, isSigner: true }, { pubkey: key(fRow.address), isWritable: true, isSigner: false },
    { pubkey: eventAuthority, isWritable: false, isSigner: false }, { pubkey: program, isWritable: false, isSigner: false },
  ], data: successorCoder.instruction.encode('update_protocol_revenue', { args: { swap_bps: null, interest_bps: null,
    max_referral_interest_share_bps: null, revenue_distribution: null, protocol_auction_split: null } }) })], 'successor preserves existing futarchy configuration and authority');
  assert.deepEqual(Buffer.from(svm.getAccount(key(fRow.address)).data), before);
  const delegateBinary = readFileSync(delegateBinaryFile), delegateIdl = JSON.parse(readFileSync(delegateIdlFile));
  const delegateCoder = new BorshCoder(delegateIdl), orderKey = key(scope.preservedOwnerOrderRentClaim.address);
  const order = delegateCoder.accounts.decode('LeverageOrder', Buffer.from(svm.getAccount(orderKey).data));
  const owner = key(scope.preservedOwnerOrderRentClaim.owner), beforeRent = svm.getBalance(owner), rent = svm.getBalance(orderKey);
  svm.addProgramWithLoader(key(delegateIdl.address), delegateBinary, loader);
  send(svm, [new TransactionInstruction({ programId: key(delegateIdl.address), keys: [
    { pubkey: orderKey, isSigner: false, isWritable: true }, { pubkey: owner, isSigner: true, isWritable: true },
  ], data: delegateCoder.instruction.encode('cancel_leverage_order', { _args: { order_id: order.order_id } }) })], 'successor preserves owner cancellation and rent recovery for empty delegate order');
  assert.equal(svm.getBalance(owner) - beforeRent, rent);
  successorCompatibility = { normalDuskSha256: hash(successorBinary).toString('hex'), delegateSha256: hash(delegateBinary).toString('hex'),
    tombstoneStillEnforced: true, futarchyPreserved: true, ownerOrderRentRecoverable: true };
}
// A true pre-begin rollback may resume ordinary owner exits. Its changed
// market must invalidate the old cutoff before a fresh freeze can be created.
const stale = runtime();
send(stale, [freezeInstruction()], 'stale-freeze case: capture cutoff');
installLocalCode(stale, originalProgramData.subarray(45));
const exited = stale.sendTransaction(VersionedTransaction.deserialize(Buffer.from(oldClose.unsignedTransaction, 'base64')));
assert.ok(!(exited instanceof FailedTransactionMetadata), exited instanceof FailedTransactionMetadata ? exited.meta().logs().join('\n') : '');
installLocalCode(stale, binary);
send(stale, [begin()], 'a resumed old market cannot settle against stale freeze evidence', 'Evidence');
send(stale, [instruction('abort_stale_legacy_freeze', {}, { authority, market, frozen, retirement: state })], 'reset genuinely stale unfunded freeze');
assert.ok(!stale.getAccount(frozen) || stale.getAccount(frozen).data.length === 0);
const result = { schemaVersion: 'dusk-retirement-program-rehearsal.v1', localOnly: true, executionReady: false,
  sourceBankSlot: source.bank.slot, sourceBankSha256: source.bankSha256, quoteReportSha256: quote.contentSha256,
  temporaryBinarySha256: hash(binary).toString('hex'), temporaryIdlSha256: hash(canonical(idl)).toString('hex'),
  limitation: 'Administrator-attested economic manifest; actual freeze, reviewed frozen recapture and live execution are separate.',
  userReceipts, rentReceipts, treasuryReceipts: { owner: scope.treasury, baseRaw: treasuryReceipts[0], quoteRaw: treasuryReceipts[1] },
  preservedOwnerOrderRentClaim: scope.preservedOwnerOrderRentClaim, checks, transactions };
if (successorCompatibility) result.successorCompatibility = successorCompatibility;
writeFileSync(outputFile, JSON.stringify({ ...result, contentSha256: hash(canonical(result)).toString('hex') }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ localOnly: true, executionReady: false, sourceBankSlot: source.bank.slot, nativeChecks: checks.length,
  localTransactions: transactions.length, usersPaid: userReceipts.length, custodyConserved: true }, null, 2));
