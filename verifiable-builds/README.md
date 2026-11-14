# Verifiable mainnet builds

Use the [mainnet release workflow and local signing instructions](../scripts/release/README.md).

`release-build.yaml` creates an immutable bundle for both Dusk and leverage
delegate, including both IDLs, both TypeScript interfaces and a source/build/hash
manifest. It runs LiteSVM against those exact production executables. It never
creates a commit, deploys, uploads an IDL, or publishes npm.

The bundle is under `target/mainnet-release` after a local reproducible build or
a download of the successful workflow artifact. `yarn release:build` requires
Docker and uses the pinned builder image. `yarn release:local plan` previews the
local encrypted-volume deployment. On-chain bytecode and IDL verification
requires an explicit mainnet RPC; deployment and public verification metadata
attestation additionally require explicit `--execute` and local signing.

Stable SDK publishing checks both on-chain executables against the tested
release bundle as well as both on-chain IDLs. Keep production artifacts outside
the source tree; this directory contains documentation only.
