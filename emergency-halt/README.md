# Dusk Emergency Halt

This directory builds a minimal sBPF program that always returns custom error
`1`. Upgrading Dusk to this artifact leaves the Dusk program ID, ProgramData
capacity, markets, positions, PDAs, and token vaults in place while making every
Dusk instruction fail closed. A later upgrade through the same Solana
Upgradeable Loader and Squads authority restores reviewed Dusk code at the same
program ID.

The halt artifact is a last resort. Use global or market reduce-only first when
it can safely stop risk-increasing actions while preserving repayment,
withdrawal, liquidation, closure, and claims.

## Activation Policy

Prepare an emergency halt proposal only when all of the following are true:

1. A critical defect presents a credible path to loss, unauthorized state
   mutation, or broken reserve/debt accounting.
2. Continued execution is more dangerous than temporarily freezing every Dusk
   operation.
3. Reduce-only is bypassed, unavailable, compromised, or insufficient to
   contain the defect.
4. The exact halt artifact hash and the current Dusk program ID have been
   independently verified.
5. A reviewed restoration binary and enough Squads signers to restore it are
   available before the halt proposal is executed.

Do not use the halt for routine maintenance, a failed deployment, parameter
disagreement, UI or indexer outages, isolated keeper failures, or incidents
that reduce-only safely contains.

## What Halting Does

The complete executable body is:

```asm
mov64 r0, 1
exit
```

Every top-level Dusk call returns `Custom(1)` without reading or modifying its
accounts. Calls through Token-2022 transfer hooks and normal integrations also
fail when they propagate that Dusk error.

Funds are not deleted or transferred, but they are intentionally inaccessible
through Dusk until restoration. Solana time and external markets continue
moving during the halt. Interest, health, expiries, and loss conditions may
therefore be worse when execution resumes.

## Reproducible Build

The build uses Anza's Solana platform-tools `v1.54`, not an upstream binary:

```bash
cargo build-sbf --install-only --tools-version v1.54
yarn check:emergency-halt
```

Expected output:

```text
target/deploy/dusk_emergency_halt.so
size: 352 bytes
SHA-256: 08672b4c1d665c79b007d72e19d98d07a6d522232410d39d82f33e2670d53800
runtime result: Custom(1)
compute units: 2
```

The check also verifies the ELF machine, entrypoint opcodes, read/execute-only
load segment, artifact hash, and that a supplied writable account remains
unchanged after invocation.

## Preparation

The **Prepare Dusk Emergency Halt Buffer** workflow builds and verifies the
artifact, writes it to a fresh Upgradeable Loader buffer, and transfers buffer
authority to the configured Squads vault on mainnet. It never upgrades Dusk.

Mainnet preparation requires:

- execution from the `main` branch;
- repository variable `DUSK_EMERGENCY_HALT_BUFFERS_ENABLED=true` for the
  approved preparation window;
- the exact confirmation phrase required by the workflow;
- `SQUADS_VAULT_ADDRESS` and the existing deployer secret; and
- recording the commit, artifact hash, and buffer address from the workflow
  summary before resetting the repository variable to `false`.

The buffer is consumed when Squads executes an upgrade. Prepare and record a
replacement after any use.

## Incident Procedure

1. Attempt reduce-only if doing so is safe and does not delay containment.
2. Confirm the observed defect satisfies every activation criterion above.
3. Confirm the prepared halt buffer authority and recorded SHA-256 through
   independent RPC and artifact checks.
4. Create and approve the Squads upgrade proposal targeting the existing Dusk
   program and prepared halt buffer.
5. Verify ordinary instructions and Token-2022 LP transfer-hook paths fail,
   publish the halt status, and begin the reviewed repair process.

Never set the Dusk upgrade authority to `None` while the halt is installed. Do
not deploy the repaired program at a new address: changing the program ID
changes its PDAs and can make existing vault authority unreachable.

## Restoration

1. Build, review, and verify the repaired Dusk release through the normal
   release checklist.
2. Confirm its buffer is controlled by the same Squads authority and fits the
   existing ProgramData capacity.
3. Upgrade the same Dusk program ID, then verify its deployed hash.
4. Test repayment, withdrawal, position closure, liquidation, claims, swaps,
   and LP transfer hooks against the preserved accounts before reopening new
   risk.
5. Keep reduce-only enabled until post-restoration accounting and custody
   reconciliation pass.

Loss of the upgrade authority, inability to reach the Squads threshold, an
incompatible restoration binary, or restoration at a different program ID can
turn the temporary freeze into permanently inaccessible funds.

## Credit

The fail-closed two-instruction design is inspired by Dean Little's
[`sbpf-asm-abort`](https://github.com/deanmlittle/sbpf-asm-abort), reviewed at
commit [`2075cc5`](https://github.com/deanmlittle/sbpf-asm-abort/commit/2075cc59f5b764557589002eedaca1055f2579a8).
That project demonstrated how small an emergency abort ELF can be. Dusk's
assembly, linker script, build pipeline, tests, policy, and binary are
independently implemented; no upstream source or binary is vendored.
