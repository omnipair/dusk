/** Offline evidence primitives. No signing, RPC transport or chain mutations. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { PublicKey } from '@solana/web3.js';
import anchor from '@coral-xyz/anchor';
import { assertProgramBytes, decodeIdl, idlHash, CONFIG } from '../release/artifacts.mjs';

export const scope = JSON.parse(readFileSync(new URL('./scope.json', import.meta.url)));
export const key = value => new PublicKey(value);
export const canonical = value => value === null || typeof value !== 'object' ? JSON.stringify(value)
  : Array.isArray(value) ? `[${value.map(canonical).join(',')}]`
    : `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(',')}}`;
export const sha = bytes => createHash('sha256').update(bytes).digest('hex');
export const digest = value => sha(canonical(value));
export const seal = body => ({ ...body, contentSha256: digest(body) });
export function readEvidence(file) {
  const result = JSON.parse(readFileSync(file)), { contentSha256, ...body } = result;
  assert.equal(digest(body), contentSha256, 'Evidence body changed'); return result;
}
export function saveEvidence(file, body) {
  const result = seal(body);
  writeFileSync(file, JSON.stringify(result, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); return result;
}
export const freezeAddress = key(PublicKey.findProgramAddressSync([Buffer.from('legacy_freeze'), key(scope.market).toBuffer()], key(scope.program))[0]).toBase58();
export const retirementAddress = PublicKey.findProgramAddressSync([Buffer.from('legacy_retirement'), key(scope.market).toBuffer()], key(scope.program))[0].toBase58();
export const CLOCK = 'SysvarC1ock11111111111111111111111111111111';
export const LOADER = 'BPFLoaderUpgradeab1e11111111111111111111111';
export const TOKEN = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
export const TOKEN22 = 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb';
export const bytes = account => Buffer.from(account.data, 'base64');
export function isIdlMetadata(data) {
  return data.length >= 44 && data.subarray(0, 8).equals(Buffer.from(sha('internal:IdlAccount'), 'hex').subarray(0, 8))
    && data.readUInt32LE(40) <= data.length - 44;
}
export function bankRows(bank) {
  assert.ok(Number.isSafeInteger(bank.slot) && bank.slot > 0 && bank.accounts.length <= 100);
  assert.equal(new Set(bank.accounts.map(row => row.address)).size, bank.accounts.length);
  const rows = new Map(bank.accounts.map(row => [row.address, row.account]));
  const clock = rows.get(CLOCK); assert.equal(clock?.owner, 'Sysvar1111111111111111111111111111111111111');
  assert.equal(bytes(clock).length, 40); assert.equal(bytes(clock).readBigUInt64LE(0), BigInt(bank.slot));
  for (const { account } of bank.accounts) if (account) {
    assert.ok(Number.isSafeInteger(account.lamports) && account.lamports >= 0);
    assert.match(account.rentEpoch, /^(0|[1-9][0-9]*)$/);
    assert.ok(BigInt(account.rentEpoch) < 1n << 64n);
  }
  return rows;
}
export async function idlAddress(program) {
  const p = key(program), [base] = PublicKey.findProgramAddressSync([], p);
  return (await PublicKey.createWithSeed(base, 'anchor:idl', p)).toBase58();
}
export function executable(rows, expected, slot, replacement) {
  const program = rows.get(expected.programId), metadata = rows.get(expected.deployment.programData);
  assert.equal(program?.owner, LOADER); assert.equal(metadata?.owner, LOADER);
  assert.ok(program.executable && !metadata.executable);
  const p = bytes(program), d = bytes(metadata);
  assert.equal(p.length, 36); assert.equal(p.readUInt32LE(0), 2);
  assert.equal(key(p.subarray(4)).toBase58(), expected.deployment.programData);
  assert.equal(d.readUInt32LE(0), 3); assert.equal(d[12], 1);
  assert.equal(key(d.subarray(13, 45)).toBase58(), scope.authority);
  assert.ok(d.readBigUInt64LE(4) <= BigInt(slot));
  if (replacement) assertProgramBytes(d, replacement);
  else {
    assert.equal(d.length, expected.deployment.allocatedBinaryBytes + 45);
    assert.equal(d.readBigUInt64LE(4), BigInt(expected.deployment.deploySlot));
    assert.equal(sha(d.subarray(45)), expected.binarySha256);
  }
  return { programId: expected.programId, deploySlot: String(d.readBigUInt64LE(4)),
    programData: expected.deployment.programData, allocatedBinarySha256: sha(d.subarray(45)) };
}
export function decodeFreeze(rows, idl) {
  const account = rows.get(freezeAddress); assert.equal(account?.owner, scope.program); assert.ok(!account.executable);
  const frozen = new anchor.BorshCoder(idl).accounts.decode('FrozenLegacyMarket', bytes(account));
  const market = rows.get(scope.market); assert.equal(market?.owner, scope.program);
  assert.equal(sha(bytes(market)), Buffer.from(frozen.market_sha256).toString('hex'), 'Frozen market changed or settlement already begun');
  const clock = Buffer.from(frozen.clock_bytes); assert.equal(clock.length, 40);
  const snapshot = Buffer.from(frozen.receipt_snapshot), expected = [...scope.receiptMints, ...scope.receiptAccounts];
  assert.equal(snapshot.readUInt32LE(0), expected.length);
  let offset = 4; const receipts = new Map();
  for (const address of expected) {
    assert.equal(key(snapshot.subarray(offset, offset + 32)).toBase58(), address); offset += 32;
    assert.equal(key(snapshot.subarray(offset, offset + 32)).toBase58(), TOKEN22); offset += 32;
    const length = snapshot.readUInt32LE(offset); offset += 4;
    assert.ok(length >= (scope.receiptMints.includes(address) ? 82 : 165) && offset + length <= snapshot.length);
    receipts.set(address, snapshot.subarray(offset, offset + length)); offset += length;
  }
  assert.equal(offset, snapshot.length);
  for (const mint of scope.receiptMints) {
    let supply = 0n;
    for (const address of scope.receiptAccounts) {
      const data = receipts.get(address), amount = data.readBigUInt64LE(64);
      if (key(data.subarray(0, 32)).toBase58() !== mint) continue;
      const owner = key(data.subarray(32, 64)).toBase58();
      assert.ok(amount === 0n || [...scope.owners, ...scope.receiptCustodyAuthorities].includes(owner)); supply += amount;
    }
    assert.equal(supply, receipts.get(mint).readBigUInt64LE(36), 'Frozen receipts do not cover full supply');
  }
  scope.vaults.forEach((address, index) => {
    const account = rows.get(address); assert.equal(account?.owner, TOKEN);
    const data = bytes(account); assert.equal(data.length, 165);
    assert.equal(key(data.subarray(0, 32)).toBase58(), scope.mints[Math.floor(index / 4)]);
    assert.equal(key(data.subarray(32, 64)).toBase58(), scope.market);
    assert.ok(data.readBigUInt64LE(64) >= BigInt(frozen.vault_amounts[index].toString()), 'Frozen custody was spent');
  });
  return { frozen, clock, receipts, cutoffSlot: Number(clock.readBigUInt64LE(0)) };
}
export async function validateFrozenCapture(capture, bundle) {
  assert.equal(capture.schemaVersion, 'dusk-retirement-frozen-capture.v1');
  assert.equal(capture.genesisHash, CONFIG.genesisHash);
  assert.equal(capture.bundleSha256, digest(bundle.manifest));
  assert.equal(capture.bankSha256, digest(capture.bank));
  const rows = bankRows(capture.bank), idl = JSON.parse(readFileSync(new URL('./idl.json', import.meta.url)));
  assert.equal(idlHash(idl), idlHash(JSON.parse(readFileSync(`${bundle.directory}/dusk.json`))));
  const actual = executable(rows, scope.legacyIdentity.dusk, capture.bank.slot, readFileSync(`${bundle.directory}/dusk.so`));
  executable(rows, scope.legacyIdentity.delegate, capture.bank.slot);
  if (!capture.localOnly) for (const [program, expected] of [[scope.program, idl], [scope.legacyIdentity.delegate.programId, null]]) {
    const account = rows.get(await idlAddress(program)); assert.equal(account?.owner, program);
    const decoded = decodeIdl(bytes(account)); assert.equal(decoded.authority, scope.authority);
    assert.equal(idlHash(decoded.idl), expected ? idlHash(expected) : scope.legacyIdentity.delegate.idlCanonicalSha256);
  }
  const freeze = decodeFreeze(rows, idl);
  assert.ok(freeze.cutoffSlot <= capture.bank.slot && BigInt(freeze.cutoffSlot) >= BigInt(actual.deploySlot));
  assert.ok(capture.localOnly === true || capture.localOnly === false);
  return { rows, idl, ...freeze };
}
