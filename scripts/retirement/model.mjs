/** Reconstruct the approved cutoff under the verified old executable. Offline only. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import anchor from '@coral-xyz/anchor';
import { readRetirementBundle } from './bundle.mjs';
import { idlHash } from '../release/artifacts.mjs';
import { scope, key, bytes, CLOCK, LOADER, TOKEN22, digest, seal, readEvidence, saveEvidence,
  executable, validateFrozenCapture, freezeAddress } from './evidence.mjs';

const jsonState = value => anchor.BN.isBN(value) ? value.toString(10) : value?.toBase58 ? value.toBase58()
  : Array.isArray(value) ? value.map(jsonState) : value && typeof value === 'object'
    ? Object.fromEntries(Object.entries(value).map(([key, v]) => [key, jsonState(v)])) : value;
export async function reconstructQuoteModel(capture, bundle, basis, blueprint, oldIdl) {
  const { rows, receipts, clock, frozen, cutoffSlot } = await validateFrozenCapture(capture, bundle);
  assert.equal(basis.bankSha256, digest(basis.bank));
  const old = new Map(basis.bank.accounts.map(row => [row.address, row.account]));
  for (const expected of [scope.legacyIdentity.dusk, scope.legacyIdentity.delegate]) executable(old, expected, basis.bank.slot);
  assert.equal(oldIdl.address, scope.program); assert.equal(idlHash(oldIdl), scope.legacyIdentity.dusk.idlCanonicalSha256);
  assert.equal(digest(blueprint.identity), digest(scope.legacyIdentity));
  assert.equal(blueprint.schemaVersion, 'dusk-legacy-retirement-inventory.v1');
  const modeled = new Map([...rows].map(([address, account]) => [address, account ? { ...account } : null]));
  const substitutions = [];
  const substitute = (address, account, reason) => {
    substitutions.push({ address, beforeSha256: digest(modeled.get(address)), afterSha256: digest(account), reason });
    modeled.set(address, account);
  };
  substitute(scope.programData, old.get(scope.programData), 'Verified deployed layout-17 executable and loader metadata; quote basis only');
  substitute(CLOCK, { ...rows.get(CLOCK), data: clock.toString('base64') }, 'Immutable on-chain freeze clock; review time does not accrue interest');
  for (const [address, data] of receipts) {
    const current = rows.get(address), prior = old.get(address);
    assert.equal(prior?.owner, TOKEN22);
    substitute(address, { ...(current ?? prior), owner: TOKEN22, executable: false, data: data.toString('base64') },
      'Exact mint/holder bytes preserved in the on-chain freeze, including later burns/closures');
  }
  scope.vaults.forEach((address, index) => {
    const data = bytes(rows.get(address)); data.writeBigUInt64LE(BigInt(frozen.vault_amounts[index].toString()), 64);
    substitute(address, { ...rows.get(address), data: data.toString('base64') }, 'Frozen funded budget; post-cutoff donations excluded');
  });
  // CPI executables must already have existed at the cutoff. Never run a
  // post-cutoff token-program upgrade while claiming the old execution model.
  for (const [address, account] of modeled) if (account?.executable && account.owner === LOADER && address !== scope.program) {
    const metadata = modeled.get(key(bytes(account).subarray(4)).toBase58());
    assert.ok(metadata && bytes(metadata).readBigUInt64LE(4) <= BigInt(cutoffSlot), 'CPI program changed after cutoff');
  }
  const sourceModel = { schemaVersion: 'dusk-retirement-quote-model.v1', kind: 'reconstructed-quote-model', localOnly: capture.localOnly,
    actualFrozenCaptureSha256: capture.contentSha256, actualBankSha256: capture.bankSha256, temporaryBundleSha256: digest(bundle.manifest),
    executableBasis: scope.legacyIdentity, cutoffSlot, frozenAccountSha256: digest(rows.get(freezeAddress)), substitutions };
  const coder = new anchor.BorshCoder(oldIdl), decoded = [], ancillary = [];
  const programs = capture.programCatalogs.map(catalog => ({ program: catalog.program, context: { slot: cutoffSlot },
    value: catalog.value.filter(row => catalog.program !== scope.program || scope.legacyProgramAccounts.includes(row.pubkey))
      .map(row => {
        const account = modeled.get(row.pubkey) ?? { ...row.account, data: row.account.data[0] };
        const data = bytes(account), definition = (catalog.program === scope.program ? oldIdl.accounts : []).find(type => data.subarray(0, 8).equals(Buffer.from(type.discriminator)));
        if (definition) decoded.push({ address: row.pubkey, kind: definition.name, state: jsonState(coder.accounts.decode(definition.name, data)) });
        else ancillary.push({ address: row.pubkey, program: catalog.program, data: data.toString('base64'), kind: blueprint.ancillary.find(item => item.address === row.pubkey)?.kind ?? 'unclassified' });
        return { pubkey: row.pubkey, account: { ...account, data: [account.data, 'base64'] } };
      }) }));
  // Decode the retained delegate order with the already reviewed inventory
  // state only when its actual bytes still match. Changed or new orders stop.
  for (const row of blueprint.decoded.filter(row => row.kind === 'LeverageOrder')) {
    const original = old.get(row.address), actual = rows.get(row.address);
    if (!actual) continue;
    assert.equal(actual.owner, scope.legacyIdentity.delegate.programId);
    assert.equal(actual.data, original.data, 'Delegate order changed; review its escrow again'); decoded.push(row);
  }
  scope.personal.forEach((accounts, index) => accounts.forEach(expected => {
    const actual = decoded.find(row => row.address === expected.address);
    if (actual) {
      assert.equal(actual.kind, expected.kind);
      assert.equal(actual.state.owner, scope.owners[index], 'Personal rent beneficiary changed; review fixed scope again');
    } else assert.ok(!rows.get(expected.address) || rows.get(expected.address).owner === '11111111111111111111111111111111'
      && bytes(rows.get(expected.address)).length === 0, 'Personal account disappeared from the complete decoder catalog');
  }));
  const tokenBanks = scope.receiptMints.map(mint => ({ program: TOKEN22, mint, bank: { context: { slot: cutoffSlot },
    value: scope.receiptAccounts.filter(address => key(receipts.get(address).subarray(0, 32)).toBase58() === mint)
      .map(pubkey => ({ pubkey, account: { ...modeled.get(pubkey), data: [modeled.get(pubkey).data, 'base64'] } })) } }));
  const summaryReceipts = tokenBanks.map(row => ({ mint: row.mint, supply: receipts.get(row.mint).readBigUInt64LE(36).toString(),
    enumeratedAmount: row.bank.value.reduce((sum, item) => sum + receipts.get(item.pubkey).readBigUInt64LE(64), 0n).toString(),
    accounts: row.bank.value.map(item => { const data = receipts.get(item.pubkey); return { address: item.pubkey,
      owner: key(data.subarray(32, 64)).toBase58(), amount: data.readBigUInt64LE(64).toString(), frozen: data[108] === 2 }; }).filter(row => row.amount !== '0') }));
  const identity = structuredClone(scope.legacyIdentity), counts = {};
  decoded.forEach(row => { counts[row.kind] = (counts[row.kind] ?? 0) + 1; });
  const modeledBank = addresses => ({ context: { slot: cutoffSlot }, value: addresses.map(address => {
    const account = modeled.get(address); assert.ok(account, 'Missing model dependency');
    return { ...account, data: [account.data, 'base64'] };
  }) });
  const custody = blueprint.custody.map(row => ({ authority: row.authority, program: row.program, bank: { context: { slot: cutoffSlot }, value: row.bank.value.map(item => {
    const account = modeled.get(item.pubkey); assert.ok(account, 'Missing model custody');
    return { pubkey: item.pubkey, account: { ...account, data: [account.data, 'base64'] } };
  }) } }));
  const sealedInventory = seal({ schemaVersion: blueprint.schemaVersion, sourceModel, identity, readOnly: true,
    atomicSnapshot: false, commitment: 'local-model-from-finalized-freeze', observedAt: new Date().toISOString(),
    slotRange: { from: cutoffSlot, through: cutoffSlot }, observations: [], programs, decoded,
    ancillary: ancillary.filter(row => !decoded.some(item => item.address === row.address)), tokenBanks, custody,
    mints: { addresses: blueprint.mints.addresses, bank: modeledBank(blueprint.mints.addresses) },
    summary: { ...blueprint.summary, counts, owners: [...scope.owners], positions: decoded.filter(row => ['LeveragePosition', 'BorrowPosition'].includes(row.kind)), receipts: summaryReceipts, settlementReady: false } });
  const bank = { slot: cutoffSlot, accounts: basis.bank.accounts.map(row => {
    assert.ok(modeled.has(row.address), 'Frozen capture omitted quote dependency');
    return { address: row.address, account: modeled.get(row.address) };
  }) };
  const reference = seal({ schemaVersion: 'dusk-legacy-close-rehearsal.v1', sourceModel, localOnly: true,
    bankKind: 'reconstructed-quote-model', observedAt: new Date().toISOString(), inventorySha256: sealedInventory.contentSha256,
    bankSha256: digest(bank), bank, assumptions: ['Explicit frozen cutoff reconstruction; not an observed RPC bank'],
    settlementReady: false, proposal: { executionReady: false, status: 'not-evaluated-quote-model' }, runs: [] });
  return { inventory: sealedInventory, reference };
}
async function main() {
  const [capturePath, bundlePath, basisPath, inventoryPath, idlPath, inventoryOut, referenceOut] = process.argv.slice(2);
  assert.equal(process.argv.length, 9, 'Expected CAPTURE BUNDLE OLD_BANK OLD_INVENTORY OLD_IDL NEW_MODEL_INVENTORY NEW_MODEL_REFERENCE');
  const result = await reconstructQuoteModel(readEvidence(capturePath), readRetirementBundle(bundlePath), readEvidence(basisPath), readEvidence(inventoryPath), JSON.parse(readFileSync(idlPath)));
  for (const [path, value] of [[inventoryOut, result.inventory], [referenceOut, result.reference]]) {
    const { contentSha256, ...body } = value; assert.equal(contentSha256, digest(body)); saveEvidence(path, body);
  }
  console.log(JSON.stringify({ localOnly: true, executionReady: false, kind: 'reconstructed-quote-model', cutoffSlot: result.reference.bank.slot }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
