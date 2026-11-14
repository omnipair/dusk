# Dusk mainnet release

Preparation and signing are separate. A merge, CI run, bundle build, or preview
never deploys a program, uploads an IDL, changes an authority, or publishes npm.

## Fixed release identities

| Role | Address |
| --- | --- |
| Dusk | `DuskWhswJYrzABDn8GqQD191X9RobRi5jhgGntXEdnKj` |
| Leverage delegate | `DeLegABhDLREbDpwwcsbkHvDS5MwqKPAw1CJKoooUXFQ` |
| Initial upgrade authority, IDL authority, fee payer | `EdYBBhu2PU84eNt6r8zPWkaxrxbXVAzVvAz3tR3XveRq` |
| Later program upgrade authority | `4JpPs9Mi11qoj6GthQPiTjUc4gXq8BAoSs3AD6NVjQUZ` |

The SDK candidate is `@omnipair/dusk-sdk@2.27.0`, a stable mainnet version.
Devnet deployments use the existing devnet configuration and a matching devnet
source/SDK release; do not point these mainnet-bound binaries at devnet using the
older deploy commands. Clients can explicitly select another program identity.

## Build and review without signing

`release-build.yaml` builds the exact PR head or pushed main commit. A manual
run accepts a full commit SHA. It does not modify versions, create commits or
tags, or publish a GitHub release. Versions and interface changes are reviewed
in the PR before the build. The workflow:

1. Builds all test fixtures and both Anchor interfaces, checking the committed SDK.
2. Builds both programs with `production`, architecture `v0`, and the immutable
   Solana Foundation 4.1.0 builder image recorded in `config.json`.
3. Records the source commit, SDK version, build arguments and SHA-256 of both
   binaries, IDLs and TypeScript interfaces in `manifest.json`.
4. Runs the complete LiteSVM suite with the compute baseline against those exact
   production binaries. A successful build job alone is insufficient.

The artifact is `dusk-mainnet-release-<commit>`. Download the artifact from a
**successful complete main push run** into `target/mainnet-release`, then
checkout that exact commit. PR bundles prove the candidate; the post-merge
bundle binds the actual main commit, including a squash or merge commit.
Artifacts expire after 90 days; retain the reviewed bundle in release storage.
Fetch `origin/main` before checking out the merged release commit locally.

For the same reproducible local build, install Docker, Anchor 0.31.1,
Solana Verify 0.4.11, and the repository's Node/Rust dependencies:

```bash
yarn release:build
node scripts/release/local.mjs plan target/mainnet-release
```

The local build requires a clean committed checkout. It writes no chain data.
The manifest pins Dusk's `GIT_REV` to that commit and `GIT_RELEASE` to the stable
SDK release version. Source verification must use its exact recorded arguments.
Do not replace release binaries with regular `anchor build` output.

## Read-only deployment preflight

```bash
export MAINNET_RPC_URL='https://api.mainnet-beta.solana.com'
node scripts/release/local.mjs preflight target/mainnet-release
```

Preflight verifies the mainnet genesis hash, both unoccupied program IDs (or
matching already-deployed binaries when resuming), the payer balance, and
current rent. The allocation is exactly each binary's length; a later upgrade can extend
the ProgramData account when required. The reported total conservatively includes both IDL
accounts and 1 SOL for fees and retries; market creation and liquidity are
separate. Fund any reported shortfall before signing. An existing program with
different bytes or a different upgrade authority is rejected by this initial
deployment runner.

When signing is about to be authorized, mount the encrypted `Dusk Deploy` image
yourself and run the signer preflight:

```bash
node scripts/release/local.mjs preflight target/mainnet-release --signers
```

Expected files, all owner-readable only:

```text
/Volumes/Dusk Deploy/private/deployer-keypair.json
/Volumes/Dusk Deploy/private/programs/dusk-keypair.json
/Volumes/Dusk Deploy/private/programs/leverage_delegate-keypair.json
```

`DUSK_DEPLOY_VOLUME` overrides the mount point. The runner validates that it is
a mounted encrypted disk image, that real signer paths stay inside that image,
and that all three public keys equal the approved identities. It never copies
or prints private keys. No deployer secret is needed in GitHub. Keep the image
ejected between signing sessions and keep the tested encrypted backup offline.

## Deploy only after explicit launch authorization

These commands are documentation for the later authorized deployment. Without
`--execute`, deploy and attest print a preview and cannot sign or write to RPC.

```bash
node scripts/release/local.mjs deploy target/mainnet-release
node scripts/release/local.mjs deploy target/mainnet-release --execute
```

Execution requires the reviewed release commit to be on `origin/main`, a clean
checkout at that commit, adequate funding, and the mounted local signers. It
deploys each program and immediately runs `scripts/v2/upload_idl.sh` with that
program's bundled IDL and the same explicit mainnet RPC. IDL creation or upgrade
uses the deployer; program upgrade and IDL authority stay with the deployer.

After an interrupted upload, retain the CLI's buffer recovery information and
resume or reclaim that buffer before paying for another one. An already fully
deployed matching binary can be skipped on rerun; IDL upload is retried. If an
IDL is missing, the release stays unpublished until it is uploaded and verified.

## Verify code, interfaces and authorities

Read-only verification compares the finalized ProgramData code byte for byte
with both bundled executables, rejects nonzero data beyond the ELF, validates
the loader/PDA binding, compares both compressed Anchor IDLs, and checks program
and IDL authorities:

```bash
node scripts/release/local.mjs verify target/mainnet-release
```

For independent public source verification, explicitly authorize this separate
local signing step **while the deployer still has upgrade authority**:

```bash
node scripts/release/local.mjs attest target/mainnet-release
node scripts/release/local.mjs attest target/mainnet-release --execute
solana-verify remote get-status DuskWhswJYrzABDn8GqQD191X9RobRi5jhgGntXEdnKj
solana-verify remote get-status DeLegABhDLREbDpwwcsbkHvDS5MwqKPAw1CJKoooUXFQ
```

Attestation first verifies the deployed bytes, then signs the build metadata
with the encrypted-volume deployer and requests a remote rebuild from the exact
public commit, image and production arguments. Submission is not completion:
check both remote verification results before opening public traffic. See the
[Solana verification guide](https://solana.com/docs/programs/verified-builds).

## Publish the stable SDK separately

Only `publish-sdk.yaml` publishes npm. Never run `npm publish` locally.
After deployment and verification are authorized and complete, rerun its main
run, or use the documented protected `dusk-sdk-v2.27.0` tag path. For stable
versions it requires all of:

- Both committed SDK IDLs match their finalized mainnet IDL accounts.
- `release-build.yaml` completed successfully for that exact main push commit,
  including the exact-production-binary LiteSVM job.
- The downloaded bundle's source, file hashes and SDK version agree.
- Both finalized on-chain binaries and approved authorities match that bundle.

It then publishes under `latest` using npm trusted publishing/provenance. An IDL
match alone cannot publish a stable package for different deployed code. Devnet
prerelease publishing keeps its existing IDL-match behavior. A merge before
deployment skips publication. If the deployment matches but the release build
is still running, rerun publishing after the complete release workflow passes.
After the authorized publish, check the run summary and npm's actual dist-tags.

## Initial configuration and later handoff

Program deployment does not create a public market or enable keepers. Initialize
futarchy with the agreed 20% protocol swap share, 20% additive interest markup,
50% referral cap, and `8s6Jdoh7tgUqmU3D2EmpNJHSvuN5U4NybpLAdsiMitwB` as the
100% futarchy revenue recipient. Market swap fees remain per-market parameters.
Auction reserve depths and streaming rates are per-asset decisions, configured
before those assets' auctions can trade. Replay/indexer pins, keeper locks,
stable SDK integration and controlled-market smoke tests remain launch steps.

Keep program upgrades, IDLs and initial futarchy administration under the local
deployer while integrations are validated. On the later requested handoff,
transfer both program upgrade authorities and futarchy administration to the
approved Squads vault. Keep IDL authority on the local deployer for the agreed
publishing workflow. Read back authorities and refresh downstream pins/locks.
The old GitHub buffer workflow is not used for this initial deployment. Before
using it for later Squads upgrades, adapt its release download to the reviewed
immutable two-program bundle; this workflow no longer creates GitHub releases.
