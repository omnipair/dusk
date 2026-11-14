import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, mkdirSync, copyFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';
import { PublicKey } from '@solana/web3.js';

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
export const CONFIG = JSON.parse(readFileSync(new URL('./config.json', import.meta.url)));
export const LOADER = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');
export const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const canonical = value => Array.isArray(value) ? value.map(canonical) : value && typeof value === 'object'
  ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
export const idlHash = value => sha256(JSON.stringify(canonical(value)));
export const git = (...args) => execFileSync('git', args, { cwd: ROOT, encoding: 'utf8' }).trim();
export const SDK_FILES = { dusk: 'idl_v2.json', leverage_delegate: 'idl_delegate.json' };
export function cargoArgs(commit, version) {
  assert.match(commit, /^[a-f0-9]{40}$/);
  assert.match(version, /^\d+\.\d+\.\d+$/);
  return ['--locked', '--features', 'production', '--config', `env.GIT_REV="${commit}"`, '--config', `env.GIT_RELEASE="v${version}"`];
}
export function assertSourceIdentity() {
  const version = JSON.parse(readFileSync(resolve(ROOT, 'packages/dusk-sdk/package.json'))).version;
  assert.match(version, /^\d+\.\d+\.\d+$/, 'Mainnet SDK must have a stable version');
  const anchor = readFileSync(resolve(ROOT, 'Anchor.toml'), 'utf8').split('[programs.mainnet]')[1]?.split('\n[')[0];
  for (const [name, id] of Object.entries(CONFIG.programs)) {
    assert.ok(readFileSync(resolve(ROOT, `programs/${name}/src/lib.rs`), 'utf8').includes(`declare_id!("${id}")`), `${name} source ID`);
    assert.ok(anchor?.includes(`${name} = "${id}"`), `${name} Anchor mainnet ID`);
    assert.equal(JSON.parse(readFileSync(resolve(ROOT, `packages/dusk-sdk/src/${SDK_FILES[name]}`))).address, id, `${name} SDK IDL ID`);
  }
  return version;
}
export function createBundle(output, binaryDirectory = resolve(ROOT, 'target/deploy')) {
  const version = assertSourceIdentity();
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Release source has tracked changes');
  const sourceCommit = git('rev-parse', 'HEAD');
  mkdirSync(output, { recursive: true });
  const programs = {};
  for (const [name, id] of Object.entries(CONFIG.programs)) {
    const row = { id };
    for (const [kind, source, file] of [
      ['binary', resolve(binaryDirectory, `${name}.so`), `${name}.so`],
      ['idl', resolve(ROOT, `target/idl/${name}.json`), `${name}.json`],
      ['types', resolve(ROOT, `target/types/${name}.ts`), `${name}.ts`],
    ]) {
      const bytes = readFileSync(source);
      assert.ok(bytes.length > 0);
      if (kind === 'binary') {
        assert.ok(bytes.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])), `${name} must be an ELF`);
        assert.ok(bytes.includes(new PublicKey(id).toBuffer()), `${name} binary must contain its compiled ID`);
        if (name === 'dusk') assert.ok(bytes.includes(Buffer.from(sourceCommit)), 'Dusk binary source revision differs');
      }
      if (kind === 'idl') {
        const idl = JSON.parse(bytes);
        assert.equal(idl.address, id);
        assert.equal(idlHash(idl), idlHash(JSON.parse(readFileSync(resolve(ROOT, `packages/dusk-sdk/src/${SDK_FILES[name]}`)))), `${name} committed IDL drift`);
      }
      copyFileSync(source, resolve(output, file));
      row[kind] = { file, size: bytes.length, sha256: sha256(bytes) };
    }
    row.maxLength = row.binary.size;
    programs[name] = row;
  }
  const manifest = { schema: 1, sourceCommit, version, cluster: CONFIG.cluster, genesisHash: CONFIG.genesisHash,
    repository: CONFIG.repository, builderImage: CONFIG.builderImage, verifierVersion: CONFIG.verifierVersion,
    arch: 'v0', cargoArgs: cargoArgs(sourceCommit, version), programs };
  writeFileSync(resolve(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  return readBundle(output);
}
export function readBundle(directory, expectedCommit) {
  const manifest = JSON.parse(readFileSync(resolve(directory, 'manifest.json')));
  assert.equal(manifest.schema, 1);
  assert.match(manifest.sourceCommit, /^[a-f0-9]{40}$/);
  if (expectedCommit) assert.equal(manifest.sourceCommit, expectedCommit, 'Release commit differs');
  for (const key of ['cluster', 'genesisHash', 'repository', 'builderImage', 'verifierVersion']) assert.equal(manifest[key], CONFIG[key], key);
  assert.equal(manifest.arch, 'v0');
  assert.deepEqual(manifest.cargoArgs, cargoArgs(manifest.sourceCommit, manifest.version));
  assert.deepEqual(Object.keys(manifest.programs).sort(), Object.keys(CONFIG.programs).sort());
  for (const [name, id] of Object.entries(CONFIG.programs)) {
    const row = manifest.programs[name];
    assert.equal(row.id, id);
    for (const [kind, extension] of [['binary', 'so'], ['idl', 'json'], ['types', 'ts']]) {
      const entry = row[kind];
      assert.equal(entry.file, `${name}.${extension}`, 'Unexpected release path');
      const bytes = readFileSync(resolve(directory, entry.file));
      assert.equal(entry.size, bytes.length);
      assert.equal(entry.sha256, sha256(bytes), `${entry.file} hash differs`);
      if (kind === 'idl') assert.equal(JSON.parse(bytes).address, id);
      if (kind === 'binary') assert.ok(bytes.includes(new PublicKey(id).toBuffer()), 'Compiled program ID differs');
    }
    assert.equal(row.maxLength, row.binary.size);
  }
  return { directory: resolve(directory), manifest };
}
export function assertProgramBytes(programData, binary) {
  assert.ok(programData.length >= 45 + binary.length, 'Deployed program is shorter than the release');
  assert.ok(programData.subarray(45, 45 + binary.length).equals(binary), 'Deployed bytecode differs from the release');
  assert.ok(programData.subarray(45 + binary.length).every(byte => byte === 0), 'Nonzero code beyond release length');
}
export function decodeIdl(data) {
  assert.ok(data.length >= 44, 'Truncated Anchor IDL');
  assert.ok(data.subarray(0, 8).equals(createHash('sha256').update('internal:IdlAccount').digest().subarray(0, 8)), 'Invalid Anchor IDL discriminator');
  const length = data.readUInt32LE(40);
  assert.ok(length > 0 && length <= data.length - 44, 'Invalid Anchor IDL length');
  return { authority: new PublicKey(data.subarray(8, 40)).toBase58(), idl: JSON.parse(inflateSync(data.subarray(44, 44 + length))) };
}
export async function verifyOnChain(connection, bundle, initialAuthorities = false) {
  assert.equal(await connection.getGenesisHash(), CONFIG.genesisHash, 'RPC is not mainnet');
  const results = [];
  for (const [name, row] of Object.entries(bundle.manifest.programs)) {
    const program = new PublicKey(row.id);
    const dataKey = PublicKey.findProgramAddressSync([program.toBuffer()], LOADER)[0];
    const [base] = PublicKey.findProgramAddressSync([], program);
    const idlKey = await PublicKey.createWithSeed(base, 'anchor:idl', program);
    const read = await connection.getMultipleAccountsInfoAndContext([program, dataKey, idlKey], { commitment: 'finalized' });
    const [account, data, idlAccount] = read.value;
    assert.ok(account?.executable && account.owner.equals(LOADER), `${name} is not an upgradeable executable`);
    assert.equal(account.data.readUInt32LE(0), 2);
    assert.ok(new PublicKey(account.data.subarray(4, 36)).equals(dataKey), 'ProgramData address differs');
    assert.ok(data?.owner.equals(LOADER) && !data.executable && data.data.length >= 45, 'Invalid ProgramData');
    assert.equal(data.data.readUInt32LE(0), 3);
    assert.equal(data.data[12], 1, 'Unexpected immutable program');
    const authority = new PublicKey(data.data.subarray(13, 45)).toBase58();
    assert.ok((initialAuthorities ? [CONFIG.deployer] : [CONFIG.deployer, CONFIG.laterUpgradeAuthority]).includes(authority), 'Unexpected program upgrade authority');
    assertProgramBytes(data.data, readFileSync(resolve(bundle.directory, row.binary.file)));
    assert.ok(idlAccount?.owner.equals(program) && !idlAccount.executable, 'Missing or invalid IDL account');
    const decoded = decodeIdl(idlAccount.data);
    assert.equal(decoded.authority, CONFIG.idlAuthority, 'Unexpected IDL authority');
    const shipped = JSON.parse(readFileSync(resolve(bundle.directory, row.idl.file)));
    assert.equal(idlHash(decoded.idl), idlHash(shipped), `${name} deployed IDL differs`);
    results.push({ name, program: row.id, programData: dataKey.toBase58(), deploymentSlot: data.data.readBigUInt64LE(4).toString(),
      checkedSlot: read.context.slot, binaryHash: row.binary.sha256, idlHash: idlHash(decoded.idl), upgradeAuthority: authority, idlAuthority: decoded.authority });
  }
  return { sourceCommit: bundle.manifest.sourceCommit, version: bundle.manifest.version, cluster: CONFIG.cluster, programs: results };
}
