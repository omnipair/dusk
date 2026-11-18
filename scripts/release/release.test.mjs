import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { PublicKey } from '@solana/web3.js';
import { CONFIG, LOADER, ROOT, cargoArgs, readBundle, sha256, verifyOnChain, decodeIdl } from './artifacts.mjs';
import { assertEncryptedMount, preflight } from './local.mjs';
import { assertRetirementComplete, upgradePreflight, SUCCESSOR_UPGRADE_ORDER, assertSuccessorActivationOrder } from './upgrade.mjs';
import anchor from '@coral-xyz/anchor';

function fixture(t) {
  const directory = mkdtempSync(resolve(tmpdir(), 'dusk-mainnet-release-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const sourceCommit = 'a'.repeat(40), version = '2.27.0';
  const manifest = { schema: 1, sourceCommit, version, cluster: CONFIG.cluster, genesisHash: CONFIG.genesisHash,
    repository: CONFIG.repository, builderImage: CONFIG.builderImage, verifierVersion: CONFIG.verifierVersion,
    arch: 'v0', cargoArgs: cargoArgs(sourceCommit, version), programs: {} };
  const accounts = new Map();
  for (const [name, id] of Object.entries(CONFIG.programs)) {
    const binary = Buffer.concat([Buffer.from([127, 69, 76, 70]), new PublicKey(id).toBuffer(), Buffer.from(name)]);
    const idl = Buffer.from(JSON.stringify({ address: id, instructions: [] }));
    const types = Buffer.from(`export type ${name} = {};`);
    const row = { id };
    for (const [kind, file, bytes] of [['binary', `${name}.so`, binary], ['idl', `${name}.json`, idl], ['types', `${name}.ts`, types]]) {
      row[kind] = { file, size: bytes.length, sha256: sha256(bytes) };
      writeFileSync(resolve(directory, file), bytes);
    }
    row.maxLength = binary.length;
    manifest.programs[name] = row;
    const program = new PublicKey(id), programData = PublicKey.findProgramAddressSync([program.toBuffer()], LOADER)[0];
    const programBytes = Buffer.alloc(36); programBytes.writeUInt32LE(2); programData.toBuffer().copy(programBytes, 4);
    accounts.set(id, { executable: true, owner: LOADER, data: programBytes });
    const data = Buffer.alloc(45 + binary.length + 100); data.writeUInt32LE(3); data.writeBigUInt64LE(100n, 4); data[12] = 1;
    new PublicKey(CONFIG.deployer).toBuffer().copy(data, 13); binary.copy(data, 45);
    accounts.set(programData.toBase58(), { executable: false, owner: LOADER, data });
    const compressed = deflateSync(idl), idlData = Buffer.alloc(44 + compressed.length);
    createHash('sha256').update('internal:IdlAccount').digest().subarray(0, 8).copy(idlData);
    new PublicKey(CONFIG.idlAuthority).toBuffer().copy(idlData, 8); idlData.writeUInt32LE(compressed.length, 40); compressed.copy(idlData, 44);
    // PublicKey.createWithSeed is async; initialize the map in the RPC stub below.
    row.testIdl = { executable: false, owner: program, data: idlData };
  }
  const idlRows = Object.fromEntries(Object.entries(manifest.programs).map(([name, row]) => { const value = row.testIdl; delete row.testIdl; return [name, value]; }));
  const save = () => writeFileSync(resolve(directory, 'manifest.json'), JSON.stringify(manifest));
  save();
  const connection = {
    getGenesisHash: async () => CONFIG.genesisHash,
    getMultipleAccountsInfoAndContext: async keys => {
      for (const [name, row] of Object.entries(manifest.programs)) {
        const program = new PublicKey(row.id), [base] = PublicKey.findProgramAddressSync([], program);
        accounts.set((await PublicKey.createWithSeed(base, 'anchor:idl', program)).toBase58(), idlRows[name]);
      }
      return { context: { slot: 101 }, value: keys.map(key => accounts.get(key.toBase58()) ?? null) };
    },
    getAccountInfo: async key => accounts.get(key.toBase58()) ?? null,
    getMinimumBalanceForRentExemption: async bytes => bytes * 100,
    getBalance: async () => 100_000_000_000,
  };
  return { directory, manifest, accounts, idlRows, connection, save, bundle: () => readBundle(directory, sourceCommit) };
}

test('fixed identities and event signer agree with the mainnet release', () => {
  const source = readFileSync(resolve(ROOT, 'programs/dusk/src/instructions/accounting.rs'), 'utf8');
  const bump = Number(source.match(/EVENT_AUTHORITY_BUMP: u8 = (\d+)/)[1]);
  assert.equal(bump, PublicKey.findProgramAddressSync([Buffer.from('__event_authority')], new PublicKey(CONFIG.programs.dusk))[1]);
  for (const [name, id] of Object.entries(CONFIG.programs)) {
    assert.ok(readFileSync(resolve(ROOT, `programs/${name}/src/lib.rs`), 'utf8').includes(`declare_id!("${id}")`));
  }
  assert.ok(readFileSync(resolve(ROOT, 'packages/dusk-sdk/src/constants.ts'), 'utf8').includes(CONFIG.programs.dusk));
  assert.ok(readFileSync(resolve(ROOT, 'packages/dusk-sdk/src/delegate.ts'), 'utf8').includes(CONFIG.programs.leverage_delegate));
});
test('additional Cargo arguments leave dependency locking to solana-verify', () => {
  const combined = ['--locked', ...cargoArgs('a'.repeat(40), '2.27.0')];
  assert.equal(combined.filter(arg => arg === '--locked').length, 1, 'Cargo rejects duplicate --locked flags');
});
test('release manifest rejects modified bytecode, another commit and path substitution', t => {
  const f = fixture(t); f.bundle();
  assert.throws(() => readBundle(f.directory, 'b'.repeat(40)), /commit differs/);
  writeFileSync(resolve(f.directory, 'dusk.so'), Buffer.from('replaced'));
  assert.throws(() => f.bundle());
  f.manifest.programs.dusk.binary.file = '../dusk.so'; f.save();
  assert.throws(() => f.bundle(), /Unexpected release path/);
});
test('verification accepts exact deployed bytes plus allocation zeros and both IDLs', async t => {
  const f = fixture(t), result = await verifyOnChain(f.connection, f.bundle(), true);
  assert.equal(result.programs.length, 2); assert.equal(result.sourceCommit, f.manifest.sourceCommit);
});
test('verification rejects the wrong cluster before account reads', async t => {
  const f = fixture(t); f.connection.getGenesisHash = async () => 'devnet';
  f.connection.getMultipleAccountsInfoAndContext = async () => { throw new Error('must not read'); };
  await assert.rejects(verifyOnChain(f.connection, f.bundle()), /RPC is not mainnet/);
});
test('verification rejects changed code and nonzero bytes in unused allocation', async t => {
  for (const trailing of [false, true]) {
    const f = fixture(t), row = f.manifest.programs.dusk;
    const dataKey = PublicKey.findProgramAddressSync([new PublicKey(row.id).toBuffer()], LOADER)[0];
    const data = f.accounts.get(dataKey.toBase58()).data;
    data[trailing ? data.length - 1 : 45] ^= 1;
    await assert.rejects(verifyOnChain(f.connection, f.bundle()), trailing ? /Nonzero code/ : /bytecode differs/);
  }
});
test('verification enforces loader ownership, program-data identity and IDL identity', async t => {
  for (const mode of ['loader', 'program-data', 'idl-owner', 'idl-content']) {
    const f = fixture(t), account = f.accounts.get(CONFIG.programs.dusk);
    if (mode === 'loader') account.owner = PublicKey.default;
    if (mode === 'program-data') account.data.fill(0, 4);
    if (mode === 'idl-owner') f.idlRows.dusk.owner = LOADER;
    if (mode === 'idl-content') f.idlRows.dusk.data.fill(0, 0, 8);
    await assert.rejects(verifyOnChain(f.connection, f.bundle()));
  }
});
test('initial authorities are deployer; later verification accepts only the approved Squads vault', async t => {
  const f = fixture(t), key = PublicKey.findProgramAddressSync([new PublicKey(CONFIG.programs.dusk).toBuffer()], LOADER)[0];
  new PublicKey(CONFIG.laterUpgradeAuthority).toBuffer().copy(f.accounts.get(key.toBase58()).data, 13);
  await verifyOnChain(f.connection, f.bundle());
  await assert.rejects(verifyOnChain(f.connection, f.bundle(), true), /upgrade authority/);
  PublicKey.default.toBuffer().copy(f.accounts.get(key.toBase58()).data, 13);
  await assert.rejects(verifyOnChain(f.connection, f.bundle()), /upgrade authority/);
});
test('IDL decoding uses the Anchor internal discriminator and rejects truncated data', t => {
  const f = fixture(t); assert.equal(decodeIdl(f.idlRows.dusk.data).authority, CONFIG.idlAuthority);
  assert.throws(() => decodeIdl(Buffer.alloc(43)), /Truncated/);
  const data = Buffer.from(f.idlRows.dusk.data); data.writeUInt32LE(0xffffffff, 40);
  assert.throws(() => decodeIdl(data), /IDL length/);
});
test('initial preflight detects occupied IDs and insufficient funding, supports exact-binary resumption', async t => {
  const f = fixture(t); let report = await preflight(f.connection, f.bundle());
  assert.ok(report.programs.every(program => program.alreadyDeployed));
  f.accounts.delete(CONFIG.programs.leverage_delegate); f.connection.getBalance = async () => 0;
  report = await preflight(f.connection, f.bundle()); assert.ok(report.shortfallLamports > 0);
  f.accounts.set(CONFIG.programs.leverage_delegate, { executable: false, owner: LOADER });
  await assert.rejects(preflight(f.connection, f.bundle()), /ID is occupied/);
});
test('signer validation requires a mounted encrypted image', () => {
  const mount = '/Volumes/Dusk Deploy';
  const images = { images: [{ 'image-path': '/tmp/deployment.dmg', 'system-entities': [{ 'mount-point': mount }] }] };
  assertEncryptedMount(images, mount, { Properties: { Encrypted: true } });
  assert.throws(() => assertEncryptedMount(images, mount, { Properties: { Encrypted: false } }), /must be encrypted/);
  assert.throws(() => assertEncryptedMount({ images: [] }, mount, {}), /Mount/);
});
test('deploy and attestation default to preview even without a signer or RPC', t => {
  const f = fixture(t);
  for (const command of ['deploy', 'attest']) {
    const env = { ...process.env }; delete env.MAINNET_RPC_URL;
    const result = JSON.parse(execFileSync(process.execPath, [resolve(ROOT, 'scripts/release/local.mjs'), command, f.directory], { env, encoding: 'utf8' }));
    assert.equal(result.mode, 'preview; no signing or network writes');
  }
});

async function retiredFixture(t) {
  const f = fixture(t), scope = JSON.parse(readFileSync(resolve(ROOT, 'scripts/retirement/scope.json')));
  const idl = JSON.parse(readFileSync(resolve(ROOT, 'scripts/retirement/idl.json'))), coder = new anchor.BorshCoder(idl);
  const program = new PublicKey(scope.program), market = new PublicKey(scope.market);
  const [state, bump] = PublicKey.findProgramAddressSync([Buffer.from('legacy_retirement'), market.toBuffer()], program);
  const zero = () => Array(32).fill(0), bn = n => new anchor.BN(n);
  const paid = { version: 1, bump, completed: true, cleaned_mask: (1 << scope.owners.length) - 1, paid_mask: (1 << scope.owners.length) - 1, source_slot: bn(100),
    manifest_sha256: Array(32).fill(1), bank_sha256: zero(), quote_sha256: zero(), owner_evidence_root: zero(),
    payouts: Array.from({length:scope.owners.length},()=>({quoted_base:bn(0),quoted_quote:bn(0),base:bn(0),quote:bn(0)})), custody:[bn(0),bn(0)], remaining:[bn(0),bn(0)] };
  const tombstone = { owner: program, executable: false, data: await coder.accounts.encode('RetiredLegacyMarket', {retirement:state,version:1}) };
  const stateAccount = { owner: program, executable: false, data: await coder.accounts.encode('LegacyRetirement', paid) };
  f.accounts.set(market.toBase58(), tombstone); f.accounts.set(state.toBase58(), stateAccount);
  f.connection.getProgramAccounts = async () => [{pubkey:market,account:tombstone},{pubkey:state,account:stateAccount}];
  const previousDirectory = resolve(f.directory,'previous'); mkdirSync(previousDirectory);
  writeFileSync(resolve(previousDirectory,'dusk.json'),JSON.stringify(idl));
  const previous = {directory:previousDirectory,manifest:{schema:'dusk-retirement-bundle.v1',sourceCommit:'b'.repeat(40)}};
  return {...f,scope,previous,paid,coder,stateAccount,tombstone,state, savePaid:async()=>{stateAccount.data=await coder.accounts.encode('LegacyRetirement',paid);}};
}

test('upgrade preflight accepts complete retirement and exact-code resumption without signing',async t=>{
  const f=await retiredFixture(t),report=await upgradePreflight(f.connection,f.bundle(),f.previous);
  assert.ok(report.programs.every(row=>row.alreadyUpgraded));assert.equal(report.peakBufferRent,0);
  assert.equal(report.retirementManifestSha256,'01'.repeat(32));
});
test('normal activation orders delegate before Dusk so their common replay start cannot omit early Dusk writes',()=>{
  assert.deepEqual(SUCCESSOR_UPGRADE_ORDER,['leverage_delegate','dusk']);
  const state=(dusk,delegate)=>[{name:'dusk',alreadyUpgraded:dusk},{name:'leverage_delegate',alreadyUpgraded:delegate}];
  assert.doesNotThrow(()=>assertSuccessorActivationOrder(state(false,false)));
  assert.doesNotThrow(()=>assertSuccessorActivationOrder(state(false,true)));
  assert.doesNotThrow(()=>assertSuccessorActivationOrder(state(true,true)));
  assert.throws(()=>assertSuccessorActivationOrder(state(true,false)),/activation gap/);
});
test('upgrade cannot bypass unfinished payouts, personal rent, residual budget or the tombstone',async t=>{
  for(const failure of ['incomplete','unpaid','unclean','residual','tombstone','rent']) {
    const f=await retiredFixture(t);
    if(failure==='incomplete')f.paid.completed=false;
    if(failure==='unpaid')f.paid.paid_mask=15;
    if(failure==='unclean')f.paid.cleaned_mask=15;
    if(failure==='residual')f.paid.remaining[0]=new anchor.BN(1);
    if(failure==='tombstone')f.tombstone.data.fill(0,0,8);
    if(failure==='rent')f.accounts.set(f.scope.personal[0][0].address,{owner:new PublicKey(f.scope.program),executable:false,data:Buffer.from([1])});
    await f.savePaid();await assert.rejects(upgradePreflight(f.connection,f.bundle(),f.previous));
  }
});
test('retirement guard rejects a foreign state owner even when the serialized flags look complete',async t=>{
  const f=await retiredFixture(t);f.stateAccount.owner=LOADER;
  assert.throws(()=>assertRetirementComplete(f.tombstone,f.stateAccount));
});
test('first successor upgrade requires the reviewed retirement code and rejects an extra legacy claim',async t=>{
  const f=await retiredFixture(t),program=new PublicKey(CONFIG.programs.dusk);
  const temporary=Buffer.concat([Buffer.from([127,69,76,70]),program.toBuffer(),Buffer.from('temporary-fixture')]);
  writeFileSync(resolve(f.previous.directory,'dusk.so'),temporary);
  const pd=PublicKey.findProgramAddressSync([program.toBuffer()],LOADER)[0],existing=f.accounts.get(pd.toBase58());
  const bytes=Buffer.alloc(45+temporary.length+100);existing.data.copy(bytes,0,0,45);temporary.copy(bytes,45);existing.data=bytes;
  const idl=readFileSync(resolve(f.previous.directory,'dusk.json')),compressed=deflateSync(idl),data=Buffer.alloc(44+compressed.length);
  createHash('sha256').update('internal:IdlAccount').digest().subarray(0,8).copy(data);
  new PublicKey(CONFIG.idlAuthority).toBuffer().copy(data,8);data.writeUInt32LE(compressed.length,40);compressed.copy(data,44);
  f.idlRows.dusk={owner:program,executable:false,data};
  const report=await upgradePreflight(f.connection,f.bundle(),f.previous);
  assert.equal(report.programs[0].alreadyUpgraded,false);assert.ok(report.peakBufferRent>0);
  f.connection.getProgramAccounts=async()=>[{pubkey:PublicKey.default,account:{owner:program,data:Buffer.from([1])}}];
  await assert.rejects(upgradePreflight(f.connection,f.bundle(),f.previous),/outside the reviewed retirement catalog/);
});
test('upgrade refuses a wrong network before inspecting any account',async t=>{
  const f=await retiredFixture(t);f.connection.getGenesisHash=async()=>'devnet';
  f.connection.getMultipleAccountsInfoAndContext=async()=>{throw Error('must not read')};
  await assert.rejects(upgradePreflight(f.connection,f.bundle(),f.previous),/not mainnet/);
});
