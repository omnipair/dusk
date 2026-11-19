import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { PublicKey } from '@solana/web3.js';
import { CONFIG, ROOT, sha256 } from '../release/artifacts.mjs';
import { readRetirementBundle, retirementCargoArgs, checkRetirementIdl } from './bundle.mjs';

function fixture(work) {
  const directory = mkdtempSync(resolve(tmpdir(), 'retirement-bundle-test-'));
  const sourceCommit = 'a'.repeat(40), files = {};
  // Format-only synthetic ELF; never executed and never deployment evidence.
  const values = { 'dusk.so': Buffer.concat([Buffer.from([127, 69, 76, 70]), new PublicKey(CONFIG.programs.dusk).toBuffer(), Buffer.from(sourceCommit)]),
    'dusk.json': readFileSync(resolve(ROOT, 'scripts/retirement/idl.json')), 'dusk.ts': Buffer.from('// synthetic type fixture\n'),
    'scope.json': readFileSync(resolve(ROOT, 'scripts/retirement/scope.json')) };
  for (const [file, bytes] of Object.entries(values)) { writeFileSync(resolve(directory, file), bytes); files[file] = { size: bytes.length, sha256: sha256(bytes) }; }
  const manifest = { schema: 'dusk-retirement-bundle.v1', sourceCommit, program: CONFIG.programs.dusk,
    cluster: CONFIG.cluster, genesisHash: CONFIG.genesisHash, repository: CONFIG.repository, builderImage: CONFIG.builderImage,
    verifierVersion: CONFIG.verifierVersion, arch: 'v0', cargoArgs: retirementCargoArgs(sourceCommit), files,
    sdkPublication: false, executionAuthorized: false, nativeRehearsalRequired: true };
  const save = () => writeFileSync(resolve(directory, 'manifest.json'), JSON.stringify(manifest));
  save();
  try { work({ directory, manifest, save }); } finally { rmSync(directory, { recursive: true, force: true }); }
}

test('retirement bundle is a separate single-program ABI and can never authorize SDK publication', () => fixture(({ directory, manifest, save }) => {
  assert.equal(readRetirementBundle(directory).manifest.program, CONFIG.programs.dusk);
  manifest.sdkPublication = true; save(); assert.throws(() => readRetirementBundle(directory));
  manifest.sdkPublication = false; manifest.executionAuthorized = true; save(); assert.throws(() => readRetirementBundle(directory));
}));
test('changed files, source revisions and production-only flags are rejected', () => fixture(({ directory, manifest, save }) => {
  manifest.cargoArgs = ['--features', 'production']; save(); assert.throws(() => readRetirementBundle(directory));
  manifest.cargoArgs = retirementCargoArgs(manifest.sourceCommit); manifest.sourceCommit = 'b'.repeat(40); save();
  assert.throws(() => readRetirementBundle(directory));
  manifest.sourceCommit = 'a'.repeat(40); save();
  writeFileSync(resolve(directory, 'dusk.so'), Buffer.from('changed')); assert.throws(() => readRetirementBundle(directory));
}));
test('normal trading IDLs, foreign program IDs and unsafe paths cannot masquerade as a retirement release', () => fixture(({ directory, manifest, save }) => {
  const idl = JSON.parse(readFileSync(resolve(directory, 'dusk.json')));
  idl.instructions.push({ name: 'swap' }); assert.throws(() => checkRetirementIdl(idl));
  idl.instructions.pop(); idl.address = CONFIG.programs.leverage_delegate; assert.throws(() => checkRetirementIdl(idl));
  manifest.files['../outside.so'] = manifest.files['dusk.so']; save(); assert.throws(() => readRetirementBundle(directory));
}));
