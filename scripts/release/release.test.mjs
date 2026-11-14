import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { PublicKey } from '@solana/web3.js';
import { CONFIG, LOADER, ROOT, cargoArgs, readBundle, sha256, verifyOnChain, decodeIdl } from './artifacts.mjs';
import { assertEncryptedMount, preflight } from './local.mjs';

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
