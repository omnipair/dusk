/** Local-only retirement operator. Every write requires explicit --execute. */
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, appendFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout } from 'node:timers/promises';
import anchor from '@coral-xyz/anchor';
import { Connection, Keypair, TransactionMessage, VersionedTransaction, ComputeBudgetProgram } from '@solana/web3.js';
import { createAssociatedTokenAccountIdempotentInstruction, TOKEN_PROGRAM_ID } from '@solana/spl-token';
import { CONFIG, ROOT, git, assertProgramBytes, decodeIdl, idlHash } from '../release/artifacts.mjs';
import { localSigners } from '../release/local.mjs';
import { readRetirementBundle } from './bundle.mjs';
import { ownerEvidenceTree } from './merkle.mjs';
import { scope, key, bytes, sha, digest, readEvidence, freezeAddress, retirementAddress, idlAddress, LOADER, TOKEN22, executable, isIdlMetadata } from './evidence.mjs';
import { compileManifest, beginArgs, retirementInstruction, payoutInstruction } from './manifest.mjs';

async function finalized(connection, target) {
  for (let i = 0; await connection.getSlot('finalized') < target; i++) {
    assert.ok(i < 30, 'Finalization delayed; retain journal and resume'); await setTimeout(2000);
  }
}
export async function inspectTemporary(connection, bundle, allowLegacy = false) {
  assert.equal(await connection.getGenesisHash(), CONFIG.genesisHash, 'Wrong network');
  const idl = JSON.parse(readFileSync(resolve(bundle.directory, 'dusk.json'))), idlKey = await idlAddress(scope.program);
  const delegate = scope.legacyIdentity.delegate;
  const addresses = [scope.program, scope.programData, idlKey, freezeAddress, retirementAddress, scope.market,
    delegate.programId, delegate.deployment.programData, await idlAddress(delegate.programId)];
  const read = await connection.getMultipleAccountsInfoAndContext(addresses.map(key), { commitment: 'finalized' });
  const [program, metadata, idlAccount, frozen, retirement, market, delegateProgram, delegateMetadata, delegateIdl] = read.value;
  const toRaw = account => account ? { ...account, owner: account.owner.toBase58(), data: account.data.toString('base64') } : null;
  executable(new Map([[delegate.programId, toRaw(delegateProgram)], [delegate.deployment.programData, toRaw(delegateMetadata)]]), delegate, read.context.slot);
  assert.equal(delegateIdl?.owner.toBase58(), delegate.programId);
  const decodedDelegateIdl = decodeIdl(delegateIdl.data);
  assert.equal(decodedDelegateIdl.authority, scope.authority);
  assert.equal(idlHash(decodedDelegateIdl.idl), delegate.idlCanonicalSha256);
  assert.ok(program?.executable && program.owner.toBase58() === LOADER);
  assert.equal(program.data.readUInt32LE(0), 2); assert.equal(key(program.data.subarray(4)).toBase58(), scope.programData);
  assert.equal(metadata?.owner.toBase58(), LOADER); assert.ok(!metadata.executable);
  assert.equal(metadata.data.readUInt32LE(0), 3); assert.equal(metadata.data[12], 1);
  assert.equal(key(metadata.data.subarray(13, 45)).toBase58(), scope.authority);
  assert.ok(metadata.data.readBigUInt64LE(4) <= BigInt(read.context.slot));
  const binary = readFileSync(resolve(bundle.directory, 'dusk.so'));
  let alreadyTemporary = true;
  try { assertProgramBytes(metadata.data, binary); }
  catch { assert.ok(allowLegacy, 'Temporary deployment differs'); alreadyTemporary = false;
    assert.equal(sha(metadata.data.subarray(45)), scope.legacyIdentity.dusk.binarySha256);
    assert.equal(metadata.data.readBigUInt64LE(4), BigInt(scope.legacyIdentity.dusk.deployment.deploySlot)); }
  assert.equal(idlAccount?.owner.toBase58(), scope.program);
  const decoded = decodeIdl(idlAccount.data); assert.equal(decoded.authority, scope.authority);
  assert.ok([...(allowLegacy ? [scope.legacyIdentity.dusk.idlCanonicalSha256] : []), ...(alreadyTemporary ? [idlHash(idl)] : [])].includes(idlHash(decoded.idl)), 'IDL is not reviewed old or temporary ABI');
  assert.equal(market?.owner.toBase58(), scope.program);
  const check = { slot: read.context.slot, idl, alreadyTemporary, idlUploaded: idlHash(decoded.idl) === idlHash(idl), frozen, retirement, market,
    allocation: metadata.data.length, deploymentSlot: String(metadata.data.readBigUInt64LE(4)) };
  if (frozen) { assert.equal(frozen.owner.toBase58(), scope.program); check.frozenState = new anchor.BorshCoder(idl).accounts.decode('FrozenLegacyMarket', frozen.data); }
  if (retirement) { assert.equal(retirement.owner.toBase58(), scope.program); check.state = new anchor.BorshCoder(idl).accounts.decode('LegacyRetirement', retirement.data); }
  return check;
}
function assertSource(bundle) {
  assert.equal(git('rev-parse', 'HEAD'), bundle.manifest.sourceCommit, 'Checkout exact reviewed source');
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Commit changes before live execution');
  git('merge-base', '--is-ancestor', bundle.manifest.sourceCommit, 'origin/main');
}
function run(command, args, env = {}) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: 'utf8', env: { ...process.env, ...env } });
  if (result.error) throw new Error(`${command} could not start`);
  for (const output of [result.stdout, result.stderr]) if (output) process.stdout.write(output.replace(/https?:\/\/[^\s"'<>]+/g, '[redacted URL]'));
  assert.equal(result.status, 0, `${command} failed; retain buffer address and resume`);
}
export function assertManifestState(check, manifest, capture) {
  assert.ok(check.alreadyTemporary && check.idlUploaded, 'Temporary binary and IDL required');
  assert.ok(check.frozen, 'Freeze is missing');
  const frozen = capture.bank.accounts.find(row => row.address === freezeAddress)?.account;
  assert.ok(frozen && check.frozen.data.equals(bytes(frozen)), 'On-chain freeze differs from reviewed capture');
  if (!check.state) {
    assert.equal(sha(check.market.data), Buffer.from(check.frozenState.market_sha256).toString('hex'));
    return;
  }
  const state = check.state;
  assert.equal(state.version, 1);
  assert.equal(Buffer.from(state.manifest_sha256).toString('hex'), manifest.onChainManifestSha256, 'Another settlement manifest is already committed');
  assert.equal(Buffer.from(state.bank_sha256).toString('hex'), manifest.frozenBankSha256);
  assert.equal(Buffer.from(state.quote_sha256).toString('hex'), manifest.quoteReportSha256);
  assert.ok(state.cleaned_mask <= (1 << scope.owners.length) - 1 && state.paid_mask <= (1 << scope.owners.length) - 1);
  assert.ok(state.cleaned_mask === (1 << scope.owners.length) - 1 || state.paid_mask === 0, 'Inconsistent settlement masks');
  const tombstone = new anchor.BorshCoder(check.idl).accounts.decode('RetiredLegacyMarket', check.market.data);
  assert.equal(tombstone.version, 1); assert.equal(tombstone.retirement.toBase58(), retirementAddress);
  for (const [side, name] of ['base', 'quote'].entries()) {
    const paid = manifest.users.reduce((sum, row, index) => sum + (state.paid_mask & 1 << index ? BigInt(row[`${name}Raw`]) : 0n), 0n);
    assert.equal(state.custody[side].toString(), manifest.custodyRaw[side]);
    assert.equal(state.remaining[side].toString(), state.completed ? '0' : String(BigInt(manifest.custodyRaw[side]) - paid));
  }
  assert.ok(!state.completed || state.cleaned_mask === (1 << scope.owners.length) - 1 && state.paid_mask === (1 << scope.owners.length) - 1);
}
async function main() {
  const args = process.argv.slice(2), command = args.shift(), bundlePath = args.shift();
  assert.ok(['plan', 'preflight', 'upgrade', 'freeze', 'settle', 'verify'].includes(command), 'Expected plan|preflight|upgrade|freeze|settle|verify BUNDLE [evidence] [--execute]');
  const execute = args.includes('--execute'); const positional = args.filter(arg => arg !== '--execute');
  assert.ok(!execute || ['upgrade', 'freeze', 'settle'].includes(command));
  const bundle = readRetirementBundle(bundlePath);
  if (command === 'plan' || ['upgrade', 'freeze', 'settle'].includes(command) && !execute) {
    console.log(JSON.stringify({ mode: 'preview; no keys, RPC or writes', command, sourceCommit: bundle.manifest.sourceCommit,
      program: scope.program, market: scope.market, authority: scope.authority, sdkPublication: false,
      phases: ['Upgrade Dusk only and immediately upload matching temporary IDL', 'Freeze cutoff, capture and rehearse exact frozen state',
        'Review immutable net manifest before irreversible begin', 'Clear all owners, pay exact net tokens, finish treasury', 'Verify receipts before normal successor upgrade'] }, null, 2)); return;
  }
  const endpoint = process.env.MAINNET_RPC_URL; assert.ok(endpoint, 'MAINNET_RPC_URL is required');
  const connection = new Connection(endpoint, 'finalized');
  let check = await inspectTemporary(connection, bundle, ['preflight', 'upgrade'].includes(command));
  if (command === 'preflight' || command === 'upgrade') {
    assert.equal(positional.length, 1, 'Expected verified old bank report for fresh scope check');
    const template = readEvidence(positional[0]);
    // Full account scope is checked immediately before replacing normal code.
    const catalog = await connection.getProgramAccounts(key(scope.program), { commitment: 'finalized' });
    const permitted = new Set([...scope.legacyProgramAccounts, freezeAddress, retirementAddress]);
    for (const row of catalog) assert.ok(permitted.has(row.pubkey.toBase58()) || isIdlMetadata(row.account.data), 'Unreviewed Dusk account blocks temporary upgrade');
    const receipts = await connection.getMultipleAccountsInfo([...scope.receiptMints, ...scope.receiptAccounts].map(key), 'finalized');
    assert.ok(receipts.every(row => row && !row.executable && row.owner.toBase58() === TOKEN22), 'Receipt scope changed; recapture before upgrading');
    scope.receiptMints.forEach((mint, index) => {
      const holders = receipts.slice(scope.receiptMints.length).filter(row => key(row.data.subarray(0, 32)).toBase58() === mint);
      assert.equal(holders.reduce((sum, row) => sum + row.data.readBigUInt64LE(64), 0n), receipts[index].data.readBigUInt64LE(36), 'Receipt supply moved outside reviewed scope');
      assert.ok(holders.every(row => row.data.readBigUInt64LE(64) === 0n || [...scope.owners, ...scope.receiptCustodyAuthorities].includes(key(row.data.subarray(32, 64)).toBase58())), 'New receipt beneficiary requires scope review');
    });
    assert.equal(template.bankSha256, digest(template.bank));
    const required = 1_000_000_000 + await connection.getMinimumBalanceForRentExemption(37 + bundle.manifest.files['dusk.so'].size)
      + await connection.getMinimumBalanceForRentExemption(16_384);
    const balance = await connection.getBalance(key(scope.authority), 'finalized');
    console.log(JSON.stringify({ slot: check.slot, alreadyTemporary: check.alreadyTemporary, idlUploaded: check.idlUploaded,
      balanceLamports: balance, requiredLamports: required, shortfallLamports: Math.max(0, required - balance) }));
    assert.ok(balance >= required, 'Fund reported deployment shortfall');
    if (command === 'preflight') return;
    assert.ok(!check.state, 'Funded settlement exists; resume it instead of upgrading again');
  }
  let manifest, capture;
  if (command === 'settle' || command === 'verify') {
    assert.equal(positional.length, 6, 'Expected MANIFEST CAPTURE MODEL_REFERENCE QUOTE MODEL_NATIVE_PROOF FROZEN_NATIVE_PROOF');
    [manifest, capture] = positional.slice(0, 2).map(readEvidence);
    assert.equal(capture.localOnly, false, 'Local witness cannot authorize a mainnet payment');
    const compiled = await compileManifest(capture, bundle, ...positional.slice(2, 5).map(readEvidence));
    assert.equal(digest(compiled), digest(manifest), 'Manifest no longer compiles from reviewed evidence');
    const exact = readEvidence(positional[5]);
    assert.equal(exact.schemaVersion, 'dusk-retirement-frozen-execution-rehearsal.v1');
    assert.equal(exact.captureSha256, capture.contentSha256); assert.equal(exact.manifestSha256, manifest.contentSha256);
    assert.equal(exact.bundleSha256, digest(bundle.manifest)); assert.equal(exact.onChainManifestSha256, manifest.onChainManifestSha256);
    assert.equal(exact.completed, true); assert.equal(exact.exactPayouts, true); assert.equal(exact.localOnly, true);
    assertManifestState(check, manifest, capture);
    if (command === 'verify') {
      assert.ok(check.state?.completed, 'Retirement is incomplete');
      const personal = await connection.getMultipleAccountsInfo(scope.personal.flat().map(row => key(row.address)), 'finalized');
      assert.ok(personal.every(row => !row || row.owner.equals(key('11111111111111111111111111111111')) && row.data.length === 0));
      console.log(JSON.stringify({ completed: true, manifestSha256: manifest.onChainManifestSha256, masks: [check.state.cleaned_mask, check.state.paid_mask] })); return;
    }
  }
  assertSource(bundle);
  assert.equal(execFileSync('solana', ['--version'], { encoding: 'utf8' }).split(' ')[1], '4.1.0', 'Use pinned CLI');
  const signers = localSigners(process.env.DUSK_DEPLOY_VOLUME ?? '/Volumes/Dusk Deploy');
  if (command === 'upgrade') {
    if (!check.alreadyTemporary) run('solana', ['program', 'deploy', resolve(bundle.directory, 'dusk.so'), '--program-id', scope.program,
      '--keypair', signers.deployer, '--fee-payer', signers.deployer, '--upgrade-authority', signers.deployer,
      '--with-compute-unit-price', '50000', '--max-sign-attempts', '50', '--use-rpc', '--url', endpoint]);
    run('scripts/v2/upload_idl.sh', [resolve(bundle.directory, 'dusk.json')], { ANCHOR_PROVIDER_URL: endpoint, ANCHOR_WALLET: signers.deployer });
    await finalized(connection, await connection.getSlot('confirmed'));
    check = await inspectTemporary(connection, bundle); assert.ok(check.idlUploaded);
    console.log(JSON.stringify({ temporaryUpgradeVerified: true, next: 'freeze; then read-only capture and frozen quote review' })); return;
  }
  const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(signers.deployer))));
  assert.equal(payer.publicKey.toBase58(), scope.authority);
  const journal = process.env.DUSK_RETIREMENT_JOURNAL; assert.ok(journal, 'Set an absolute private release journal path'); assert.ok(resolve(journal) === journal);
  const send = async (label, instructions) => {
    const latest = await inspectTemporary(connection, bundle);
    if (manifest) assertManifestState(latest, manifest, capture);
    const blockhash = await connection.getLatestBlockhash('confirmed');
    const transaction = new VersionedTransaction(new TransactionMessage({ payerKey: payer.publicKey, recentBlockhash: blockhash.blockhash,
      instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }), ...instructions] }).compileToV0Message());
    assert.ok(transaction.serialize().length <= 1232, 'Transaction exceeds packet size'); transaction.sign([payer]);
    const simulated = await connection.simulateTransaction(transaction, { sigVerify: true, commitment: 'confirmed' });
    assert.equal(simulated.value.err, null, `${label} simulation rejected; retain journal`);
    appendFileSync(journal, JSON.stringify({ label, status: 'prepared', signatureBytes: Buffer.from(transaction.signatures[0]).toString('base64'),
      manifestSha256: manifest?.onChainManifestSha256 ?? null, blockhash }) + '\n', { mode: 0o600 });
    const signature = await connection.sendRawTransaction(transaction.serialize(), { skipPreflight: false, preflightCommitment: 'confirmed', maxRetries: 3 });
    const confirmed = await connection.confirmTransaction({ ...blockhash, signature }, 'finalized');
    appendFileSync(journal, JSON.stringify({ label, signature, error: confirmed.value.err }) + '\n', { mode: 0o600 });
    assert.equal(confirmed.value.err, null, `${label} failed; retain journal`);
    console.log(JSON.stringify({ label, signature }));
  };
  if (command === 'freeze') {
    assert.equal(positional.length, 0);
    assert.ok(!check.retirement, 'Settlement already begun');
    if (!check.frozen) await send('freeze immutable cutoff', [retirementInstruction(check.idl, 'freeze_legacy_market', {}, {},
      [...scope.receiptMints, ...scope.receiptAccounts, ...scope.vaults].map(address => ({ pubkey: key(address), isSigner: false, isWritable: false })))]);
    console.log(JSON.stringify({ frozen: true, next: 'capture; build explicit quote model; review net manifest' })); return;
  }
  if (!check.state) await send('begin immutable funded settlement', [retirementInstruction(check.idl, 'begin_legacy_retirement', { args: beginArgs(manifest) }, {},
    [...scope.vaults.map(address => ({ pubkey: key(address), isSigner: false, isWritable: true })), ...scope.mints.map(address => ({ pubkey: key(address), isSigner: false, isWritable: false }))])]);
  for (let index = 0; index < scope.owners.length; index++) {
    check = await inspectTemporary(connection, bundle); assertManifestState(check, manifest, capture);
    if (!(check.state.cleaned_mask & 1 << index)) await send(`clear owner ${index}`, [retirementInstruction(check.idl, 'clear_legacy_owner', { owner_index: index, proof: ownerEvidenceTree(manifest.users.map(row => Buffer.from(row.personalSha256, 'hex'))).proofs[index] },
      { owner: scope.owners[index] }, scope.personal[index].map(row => ({ pubkey: key(row.address), isSigner: false, isWritable: true })))]);
  }
  for (let index = 0; index <= scope.owners.length; index++) {
    check = await inspectTemporary(connection, bundle); assertManifestState(check, manifest, capture);
    const treasury = index === scope.owners.length, row = treasury ? manifest.treasury : manifest.users[index];
    if (treasury ? check.state.completed : check.state.paid_mask & 1 << index) continue;
    const creates = scope.mints.map((mint, side) => createAssociatedTokenAccountIdempotentInstruction(payer.publicKey,
      key(row[side ? 'quoteRecipient' : 'baseRecipient']), key(row.owner), key(mint), TOKEN_PROGRAM_ID));
    await send(treasury ? 'finish treasury' : `pay owner ${index}`, [...creates, payoutInstruction(check.idl, manifest, index)]);
  }
  console.log(JSON.stringify({ status: 'submitted and finalized; run read-only verify before successor upgrade' }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
