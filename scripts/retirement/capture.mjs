/** Finalized read-only capture after the temporary binary's freeze. */
import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { readRetirementBundle } from './bundle.mjs';
import { CONFIG } from '../release/artifacts.mjs';
import { readEvidence, saveEvidence, digest, scope, freezeAddress, retirementAddress, idlAddress,
  validateFrozenCapture, executable, isIdlMetadata } from './evidence.mjs';

export function parseRpc(text) {
  return JSON.parse(text, (key, value, context) => {
    if (key !== 'rentEpoch') return value;
    assert.match(context?.source ?? '', /^(0|[1-9][0-9]*)$/);
    assert.ok(BigInt(context.source) < 1n << 64n); return context.source;
  });
}
export function readOnlyRpc(endpoint) {
  return async (method, params = []) => {
    assert.ok(['getGenesisHash', 'getMultipleAccounts', 'getProgramAccounts'].includes(method), 'Read-only transport');
    let response;
    try { response = await fetch(endpoint, { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }), signal: AbortSignal.timeout(30_000) }); }
    catch { throw new Error('Read-only capture transport failed'); }
    assert.ok(response.ok, `Read-only capture HTTP ${response.status}`);
    const body = parseRpc(await response.text()); assert.ok(!body.error, 'Read-only capture RPC rejected'); return body.result;
  };
}
export async function captureFrozen(rpc, bundle, template) {
  assert.equal(await rpc('getGenesisHash'), CONFIG.genesisHash, 'Wrong network');
  assert.equal(template.schemaVersion, 'dusk-legacy-close-rehearsal.v1');
  assert.equal(template.bankSha256, digest(template.bank));
  const oldRows = new Map(template.bank.accounts.map(row => [row.address, row.account]));
  for (const expected of [scope.legacyIdentity.dusk, scope.legacyIdentity.delegate]) executable(oldRows, expected, template.bank.slot);
  const ids = [scope.program, scope.legacyIdentity.delegate.programId];
  const catalog = async minimum => Promise.all(ids.map(async program => ({ program, ...await rpc('getProgramAccounts',
    [program, { encoding: 'base64', withContext: true, commitment: 'finalized', ...(minimum ? { minContextSlot: minimum } : {}) }]) })));
  const before = await catalog();
  const permitted = new Set([...scope.legacyProgramAccounts, freezeAddress, await idlAddress(scope.program)]);
  for (const row of before[0].value) assert.ok(permitted.has(row.pubkey) || isIdlMetadata(Buffer.from(row.account.data[0], 'base64')), 'Unreviewed Dusk account; capture requires scope review');
  const priorDelegate = new Set([...template.bank.accounts.filter(row => row.account?.owner === ids[1]).map(row => row.address), await idlAddress(ids[1])]);
  for (const row of before[1].value) assert.ok(priorDelegate.has(row.pubkey) || isIdlMetadata(Buffer.from(row.account.data[0], 'base64')), 'Unreviewed delegate account');
  const addresses = [...new Set([...template.bank.accounts.map(row => row.address), freezeAddress, retirementAddress,
    ...await Promise.all(ids.map(idlAddress))])].sort();
  assert.ok(addresses.length <= 100, 'Capture exceeds one atomic account read');
  const minimum = Math.max(...before.map(row => row.context.slot));
  const captured = await rpc('getMultipleAccounts', [addresses, { encoding: 'base64', commitment: 'finalized', minContextSlot: minimum }]);
  assert.equal(captured.value.length, addresses.length); assert.ok(captured.context.slot >= minimum);
  const bank = { slot: captured.context.slot, accounts: captured.value.map((raw, index) => {
    if (raw) assert.equal(raw.data[1], 'base64');
    return { address: addresses[index], account: raw ? { ...raw, data: raw.data[0] } : null };
  }) };
  const after = await catalog(bank.slot);
  // No normal Dusk writes exist in the temporary build. Any catalog/data
  // change during capture requires a new coherent read, never arrival-order selection.
  const stable = value => value.map(row => [row.pubkey, row.account.owner, row.account.executable, row.account.data]).sort((a, b) => a[0].localeCompare(b[0]));
  before.forEach((old, index) => assert.equal(digest(stable(old.value)), digest(stable(after[index].value)), 'Program catalog changed during capture'));
  const rows = new Map(bank.accounts.map(row => [row.address, row.account]));
  for (const observation of before) for (const row of observation.value) if (rows.has(row.pubkey)) {
    const actual = rows.get(row.pubkey); assert.equal(actual?.owner, row.account.owner);
    assert.equal(actual?.data, row.account.data[0], 'Atomic read differs from stable program catalog');
  }
  const capture = { schemaVersion: 'dusk-retirement-frozen-capture.v1', localOnly: false, readOnly: true, executionReady: false,
    observedAt: new Date().toISOString(), genesisHash: CONFIG.genesisHash, commitment: 'finalized',
    bundleSha256: digest(bundle.manifest), bankSha256: digest(bank), bank,
    programCatalogs: before, catalogVerificationSlots: after.map(row => row.context.slot),
    provenance: 'Actual RPC account bank; catalogs are separate finalized observations checked before and after the bank.' };
  assert.ok(!rows.get(retirementAddress), 'Funded retirement already begun; use the immutable manifest to resume');
  await validateFrozenCapture(capture, bundle); return capture;
}
async function main() {
  const [bundlePath, templatePath, output] = process.argv.slice(2);
  assert.equal(process.argv.length, 5, 'Expected RETIREMENT_BUNDLE VERIFIED_OLD_BANK_REPORT NEW_CAPTURE');
  assert.ok(process.env.MAINNET_RPC_URL, 'MAINNET_RPC_URL is required');
  const capture = await captureFrozen(readOnlyRpc(process.env.MAINNET_RPC_URL), readRetirementBundle(resolve(bundlePath)), readEvidence(templatePath));
  saveEvidence(output, capture);
  console.log(JSON.stringify({ readOnly: true, executionReady: false, slot: capture.bank.slot, accounts: capture.bank.accounts.length }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
