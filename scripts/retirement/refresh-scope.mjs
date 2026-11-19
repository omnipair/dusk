/** Refresh public fixed addresses from reviewed local legacy discovery evidence. */
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { readEvidence, scope, digest } from './evidence.mjs';
import { idlHash } from '../release/artifacts.mjs';

assert.equal(process.argv.length, 4, 'Expected REVIEWED_INVENTORY VERIFIED_OLD_IDL');
const inventory = readEvidence(process.argv[2]), idl = JSON.parse(readFileSync(process.argv[3]));
assert.equal(inventory.schemaVersion, 'dusk-legacy-retirement-inventory.v1');
assert.equal(digest(inventory.identity), digest(scope.legacyIdentity));
assert.equal(idlHash(idl), scope.legacyIdentity.dusk.idlCanonicalSha256);
assert.equal(inventory.summary.market, scope.market);
assert.equal(inventory.decoded.filter(row => row.kind === 'Market').length, 1);
assert.ok(!inventory.ancillary.some(row => row.kind === 'unclassified'));
const owners = [...inventory.summary.owners].sort(); assert.ok(owners.length > 0 && owners.length <= 8);
const personalKinds = ['LeveragePosition', 'LeverageDelegation', 'YieldAccount', 'JuniorExitAccount', 'BorrowPosition'];
const personal = owners.map(owner => inventory.decoded.filter(row => personalKinds.includes(row.kind) && row.state.owner === owner)
  .sort((a, b) => a.address.localeCompare(b.address)).map(row => ({ address: row.address, kind: row.kind,
    discriminator: idl.accounts.find(type => type.name === row.kind).discriminator })));
assert.equal(personal.flat().length, inventory.decoded.filter(row => personalKinds.includes(row.kind)).length);
const updated = { ...scope, owners, personal, receiptMints: inventory.summary.receipts.map(row => row.mint).sort(),
  receiptAccounts: inventory.tokenBanks.flatMap(row => row.bank.value.map(item => item.pubkey)).sort(),
  legacyProgramAccounts: inventory.programs.find(row => row.program === scope.program).value.map(row => row.pubkey).sort(),
  scopeInventorySha256: inventory.contentSha256 };
for (const values of [updated.owners, updated.personal.flat().map(row => row.address), updated.receiptAccounts, updated.legacyProgramAccounts])
  assert.equal(new Set(values).size, values.length);
const lines = ['// Public fixed account scope from reviewed legacy inventory. No amounts or signing keys.', 'use anchor_lang::prelude::*;'];
for (const [name, field] of [['MARKET', 'market'], ['AUTHORITY', 'authority'], ['PROGRAM_DATA', 'programData'], ['TREASURY', 'treasury']])
  lines.push(`pub const ${name}: Pubkey = pubkey!("${updated[field]}");`);
const keys = (name, values) => lines.push(`pub const ${name}: [Pubkey; ${values.length}] = [`, ...values.map(value => `pubkey!("${value}"),`), '];');
keys('OWNERS', owners); keys('MINTS', updated.mints);
lines.push(`pub const PARAMS_HASH: [u8; 32] = [${updated.paramsHash.join(', ')}];`, `pub const MARKET_BUMP: u8 = ${updated.marketBump};`);
keys('VAULTS', updated.vaults);
lines.push('#[derive(Clone, Copy)]', 'pub struct PersonalAccount { pub address: Pubkey, pub discriminator: [u8; 8] }',
  `pub const PERSONAL: [&[PersonalAccount]; ${owners.length}] = [`);
for (const group of personal) lines.push('&[', ...group.map(row => `PersonalAccount { address: pubkey!("${row.address}"), discriminator: [${row.discriminator.join(', ')}] }, // ${row.kind}`), '],');
lines.push('];');
keys('RECEIPT_MINTS', updated.receiptMints); keys('RECEIPT_ACCOUNTS', updated.receiptAccounts); keys('RECEIPT_CUSTODY', updated.receiptCustodyAuthorities);
writeFileSync(new URL('./scope.json', import.meta.url), JSON.stringify(updated, null, 2) + '\n');
writeFileSync(resolve('programs/dusk/src/retirement/scope.rs'), lines.join('\n') + '\n');
console.log(JSON.stringify({ owners: owners.length, personalAccounts: personal.flat().length, receiptAccounts: updated.receiptAccounts.length, executionReady: false }));
