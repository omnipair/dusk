/** Reproducible unsigned candidate only. Never deploys or publishes an SDK. */
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { CONFIG, ROOT, git } from '../release/artifacts.mjs';
import { prepareBuild } from '../prepare_build.js';
import { createRetirementBundle, retirementCargoArgs } from './bundle.mjs';

const run = (command, args) => {
  const result = spawnSync(command, args, { cwd: ROOT, stdio: 'inherit' });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed`);
};
assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Retirement build requires committed reviewed source');
assert.equal(execFileSync('solana-verify', ['--version'], { encoding: 'utf8' }).trim(), `solana-verify ${CONFIG.verifierVersion}`);
const commit = git('rev-parse', 'HEAD'), artifacts = resolve(ROOT, 'target/retirement-build');
prepareBuild(ROOT); mkdirSync(artifacts, { recursive: true });
run('anchor', ['idl', 'build', '-p', 'dusk', '-o', resolve(artifacts, 'dusk.json'), '-t', resolve(artifacts, 'dusk.ts'), '--', '--features', 'production,legacy-retirement']);
run('cargo', ['test', '-p', 'dusk', '--lib', '--features', 'production,legacy-retirement']);
run('solana-verify', ['build', '--base-image', CONFIG.builderImage, '--arch', 'v0', '--library-name', 'dusk', '--', ...retirementCargoArgs(commit)]);
copyFileSync(resolve(ROOT, 'target/deploy/dusk.so'), resolve(artifacts, 'dusk.so'));
const bundle = createRetirementBundle(resolve(ROOT, 'target/retirement-release'), artifacts);
console.log(JSON.stringify({ sourceCommit: bundle.manifest.sourceCommit, bundle: bundle.directory,
  signing: false, publication: false, status: 'candidate; exact-binary frozen-state rehearsal and execution authorization still required' }, null, 2));
