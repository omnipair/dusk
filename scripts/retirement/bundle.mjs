import assert from 'node:assert/strict';
import { readFileSync, mkdirSync, copyFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { PublicKey } from '@solana/web3.js';
import { CONFIG, ROOT, git, idlHash, sha256 } from '../release/artifacts.mjs';

export const RETIREMENT_INSTRUCTIONS = ['abort_stale_legacy_freeze', 'begin_legacy_retirement', 'clear_legacy_owner',
  'finish_legacy_retirement', 'freeze_legacy_market', 'pay_legacy_owner'];
export function retirementCargoArgs(commit) {
  assert.match(commit, /^[a-f0-9]{40}$/);
  return ['--features', 'production,legacy-retirement', '--config', `env.GIT_REV="${commit}"`,
    '--config', `env.GIT_RELEASE="retirement-${commit.slice(0, 12)}"`];
}
export function checkRetirementIdl(idl) {
  assert.equal(idl.address, CONFIG.programs.dusk);
  assert.deepEqual(idl.instructions.map(row => row.name).sort(), RETIREMENT_INSTRUCTIONS);
}
export function createRetirementBundle(output, artifacts) {
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Commit retirement source before building');
  const commit = git('rev-parse', 'HEAD');
  const idl = JSON.parse(readFileSync(resolve(artifacts, 'dusk.json')));
  checkRetirementIdl(idl);
  assert.equal(idlHash(idl), idlHash(JSON.parse(readFileSync(resolve(ROOT, 'scripts/retirement/idl.json')))), 'Retirement interface drift');
  const binary = readFileSync(resolve(artifacts, 'dusk.so'));
  assert.ok(binary.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])));
  assert.ok(binary.includes(new PublicKey(CONFIG.programs.dusk).toBuffer()));
  assert.ok(binary.includes(Buffer.from(commit)), 'Retirement binary source revision differs');
  mkdirSync(output, { recursive: true });
  const files = {};
  for (const file of ['dusk.so', 'dusk.json', 'dusk.ts']) {
    const bytes = readFileSync(resolve(artifacts, file));
    copyFileSync(resolve(artifacts, file), resolve(output, file));
    files[file] = { size: bytes.length, sha256: sha256(bytes) };
  }
  const scope = readFileSync(resolve(ROOT, 'scripts/retirement/scope.json'));
  writeFileSync(resolve(output, 'scope.json'), scope);
  files['scope.json'] = { size: scope.length, sha256: sha256(scope) };
  const manifest = { schema: 'dusk-retirement-bundle.v1', sourceCommit: commit, program: CONFIG.programs.dusk,
    cluster: CONFIG.cluster, genesisHash: CONFIG.genesisHash, repository: CONFIG.repository, builderImage: CONFIG.builderImage,
    verifierVersion: CONFIG.verifierVersion, arch: 'v0', cargoArgs: retirementCargoArgs(commit), files,
    sdkPublication: false, executionAuthorized: false, nativeRehearsalRequired: true };
  writeFileSync(resolve(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  return readRetirementBundle(output);
}
export function readRetirementBundle(directory) {
  const manifest = JSON.parse(readFileSync(resolve(directory, 'manifest.json')));
  assert.equal(manifest.schema, 'dusk-retirement-bundle.v1');
  assert.equal(manifest.program, CONFIG.programs.dusk);
  for (const field of ['cluster', 'genesisHash', 'repository', 'builderImage', 'verifierVersion']) assert.equal(manifest[field], CONFIG[field]);
  assert.equal(manifest.arch, 'v0'); assert.equal(manifest.sdkPublication, false);
  assert.equal(manifest.executionAuthorized, false); assert.equal(manifest.nativeRehearsalRequired, true);
  assert.deepEqual(manifest.cargoArgs, retirementCargoArgs(manifest.sourceCommit));
  assert.deepEqual(Object.keys(manifest.files).sort(), ['dusk.json', 'dusk.so', 'dusk.ts', 'scope.json']);
  for (const file of Object.keys(manifest.files)) {
    const bytes = readFileSync(resolve(directory, file));
    assert.equal(bytes.length, manifest.files[file].size); assert.equal(sha256(bytes), manifest.files[file].sha256, `${file} changed`);
  }
  const binary = readFileSync(resolve(directory, 'dusk.so'));
  assert.ok(binary.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])));
  assert.ok(binary.includes(new PublicKey(manifest.program).toBuffer()));
  assert.ok(binary.includes(Buffer.from(manifest.sourceCommit)));
  checkRetirementIdl(JSON.parse(readFileSync(resolve(directory, 'dusk.json'))));
  assert.equal(sha256(readFileSync(resolve(directory, 'scope.json'))), sha256(readFileSync(resolve(ROOT, 'scripts/retirement/scope.json'))));
  return { directory: resolve(directory), manifest };
}
