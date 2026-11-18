import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import anchor from '@coral-xyz/anchor';
import { ComputeBudgetProgram, TransactionMessage, VersionedTransaction } from '@solana/web3.js';
import { scope, key, sha, digest, seal, bankRows, decodeFreeze, freezeAddress, CLOCK, TOKEN, TOKEN22 } from './evidence.mjs';
import { parseRpc, readOnlyRpc } from './capture.mjs';
import { minimum, beginArgs, ownerEvidence, retirementInstruction } from './manifest.mjs';
import { assertManifestState } from './local.mjs';

const idl = JSON.parse(readFileSync(new URL('./idl.json', import.meta.url))), coder = new anchor.BorshCoder(idl);
const encode = async (name, value) => { const entry = coder.accounts.accountLayouts.get(name), buffer = Buffer.alloc(9000);
  const length = entry.layout.encode(value, buffer); return Buffer.concat([Buffer.from(entry.discriminator), buffer.subarray(0, length)]); };
const raw = (data, owner = scope.program) => ({ owner, executable: false, lamports: 1, rentEpoch: '0', data: Buffer.from(data).toString('base64') });
async function fixture() {
  const market = Buffer.alloc(100), clock = Buffer.alloc(40); clock.writeBigUInt64LE(99n);
  const rows = new Map([[scope.market, raw(market)]]), parts = [], count = Buffer.alloc(4);
  count.writeUInt32LE(scope.receiptMints.length + scope.receiptAccounts.length); parts.push(count);
  for (const address of [...scope.receiptMints, ...scope.receiptAccounts]) {
    const mint = scope.receiptMints.includes(address), data = Buffer.alloc(mint ? 82 : 165), length = Buffer.alloc(4);
    if (!mint) { key(scope.receiptMints[0]).toBuffer().copy(data); key(scope.owners[0]).toBuffer().copy(data, 32); data[108] = 1; }
    length.writeUInt32LE(data.length); parts.push(key(address).toBuffer(), key(TOKEN22).toBuffer(), length, data);
  }
  scope.vaults.forEach((address, index) => {
    const data = Buffer.alloc(165); key(scope.mints[Math.floor(index / 4)]).toBuffer().copy(data); key(scope.market).toBuffer().copy(data, 32);
    data.writeBigUInt64LE(100n, 64); rows.set(address, raw(data, TOKEN));
  });
  const frozen = { bump: 1, market_sha256: [...Buffer.from(sha(market), 'hex')], clock_bytes: [...clock],
    vault_amounts: Array.from({ length: 8 }, () => new anchor.BN(99)), receipt_snapshot: Buffer.concat(parts) };
  rows.set(freezeAddress, raw(await encode('FrozenLegacyMarket', frozen)));
  return { rows, frozen, clock };
}
test('RPC u64 sentinel remains exact; signing methods cannot reach transport', async () => {
  assert.equal(parseRpc('{"rentEpoch":18446744073709551615}').rentEpoch, '18446744073709551615');
  assert.throws(() => parseRpc('{"rentEpoch":18446744073709551616}'));
  await assert.rejects(readOnlyRpc('invalid endpoint')('sendTransaction', []), /Read-only transport/);
});
test('bank clock and duplicate/account precision guards fail closed', () => {
  const clock = Buffer.alloc(40); clock.writeBigUInt64LE(99n);
  const account = raw(clock, 'Sysvar1111111111111111111111111111111111111');
  const bank = { slot: 99, accounts: [{ address: CLOCK, account }] };
  assert.equal(bankRows(bank).size, 1);
  assert.throws(() => bankRows({ ...bank, slot: 100 }));
  assert.throws(() => bankRows({ ...bank, accounts: [...bank.accounts, ...bank.accounts] }));
  assert.throws(() => bankRows({ ...bank, accounts: [{ address: CLOCK, account: { ...account, lamports: Number.MAX_SAFE_INTEGER + 1 } }] }));
});
test('freeze decoder uses preserved receipt bytes; rejects changed market, funding, owners or trailing data', async () => {
  const { rows, frozen } = await fixture();
  assert.equal(decodeFreeze(rows, idl).cutoffSlot, 99);
  const changed = new Map(rows); changed.set(scope.market, raw(Buffer.alloc(100, 1)));
  assert.throws(() => decodeFreeze(changed, idl), /Frozen market changed/);
  const vault = Buffer.from(rows.get(scope.vaults[0]).data, 'base64'); vault.writeBigUInt64LE(98n, 64);
  changed.set(scope.market, rows.get(scope.market)); changed.set(scope.vaults[0], raw(vault, TOKEN));
  assert.throws(() => decodeFreeze(changed, idl), /custody was spent/);
  const bad = { ...frozen, receipt_snapshot: Buffer.concat([frozen.receipt_snapshot, Buffer.from([0])]) };
  changed.set(scope.vaults[0], rows.get(scope.vaults[0])); changed.set(freezeAddress, raw(await encode('FrozenLegacyMarket', bad)));
  assert.throws(() => decodeFreeze(changed, idl));
});
test('personal evidence hashes complete ordered bytes, excludes donated SOL rent', () => {
  const rows = new Map(scope.personal[0].map(row => [row.address, raw(Buffer.from(row.discriminator))]));
  const before = ownerEvidence(rows, 0), address = scope.personal[0][0].address;
  rows.set(address, { ...rows.get(address), lamports: 5 }); assert.equal(ownerEvidence(rows, 0), before);
  rows.delete(address); assert.throws(() => ownerEvidence(rows, 0), /omitted/);
});
test('integer minimum and CPI begin fit the real packet with heap request', () => {
  assert.equal(minimum('1'), '1'); assert.equal(minimum('101'), '100');
  assert.equal(minimum('18446744073709551615'), '18262276632972456099');
  const manifest = { sourceSlot: 99, frozenBankSha256: '01'.repeat(32), quoteReportSha256: '02'.repeat(32),
    users: scope.owners.map(owner => ({ owner, personalSha256: '03'.repeat(32), baseRaw: '100', quoteRaw: '200' })) };
  const ix = retirementInstruction(idl, 'begin_legacy_retirement', { args: beginArgs(manifest) }, {},
    [...scope.vaults.map(address => ({ pubkey: key(address), isWritable: true, isSigner: false })),
      ...scope.mints.map(address => ({ pubkey: key(address), isWritable: false, isSigner: false }))]);
  const transaction = new VersionedTransaction(new TransactionMessage({ payerKey: key(scope.authority), recentBlockhash: key(scope.market).toBase58(),
    instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.requestHeapFrame({ bytes: 262144 }), ix] }).compileToV0Message());
  assert.ok(transaction.serialize().length <= 1232);
  assert.ok(idl.instructions.find(row => row.name === 'begin_legacy_retirement').accounts.some(row => row.name === 'event_authority'));
});
test('runtime rejects changed frozen evidence and an unreviewed committed plan', async () => {
  const { rows, frozen } = await fixture(), frozenAccount = rows.get(freezeAddress);
  const capture = { bank: { accounts: [{ address: freezeAddress, account: frozenAccount }] } };
  const check = { alreadyTemporary: true, idlUploaded: true, idl, frozen: { data: Buffer.from(frozenAccount.data, 'base64') },
    frozenState: frozen, market: { data: Buffer.from(rows.get(scope.market).data, 'base64') } };
  assert.doesNotThrow(() => assertManifestState(check, {}, capture));
  assert.throws(() => assertManifestState({ ...check, frozen: { data: Buffer.alloc(1) } }, {}, capture), /differs/);
  assert.throws(() => assertManifestState({ ...check, state: { version: 1, manifest_sha256: Buffer.alloc(32) } },
    { onChainManifestSha256: '01'.repeat(32) }, capture), /Another settlement/);
});
test('evidence root is over its body and contains no self-reference', () => {
  const { contentSha256, ...body } = seal({ kind: 'local-model', source: '01'.repeat(32) });
  assert.equal(contentSha256, digest(body)); assert.notEqual(contentSha256, digest({ ...body, source: '02'.repeat(32) }));
});
