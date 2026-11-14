import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { CONFIG, ROOT, assertSourceIdentity, cargoArgs, createBundle, git } from './artifacts.mjs';
import { prepareBuild } from '../prepare_build.js';

function run(command, args) {
  const result = spawnSync(command, args, { cwd: ROOT, stdio: 'inherit' });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed`);
}
const version = assertSourceIdentity();
assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Commit the reviewed release source before building');
const commit = git('rev-parse', 'HEAD');
run('docker', ['info']);
assert.equal(execFileSync('solana-verify', ['--version'], { encoding: 'utf8' }).trim(), `solana-verify ${CONFIG.verifierVersion}`);
prepareBuild(ROOT);
for (const name of Object.keys(CONFIG.programs)) run('anchor', ['build', '-p', name, '--', '--features', 'production']);
run('npm', ['run', 'check-idl-current', '--prefix', 'packages/dusk-sdk']);
for (const name of Object.keys(CONFIG.programs)) {
  run('solana-verify', ['build', '--base-image', CONFIG.builderImage, '--arch', 'v0', '--library-name', name, '--', ...cargoArgs(commit, version)]);
}
createBundle(resolve(ROOT, 'target/mainnet-release'));
console.log(`Mainnet release bundle: target/mainnet-release (${commit}). No network writes or publication.`);
