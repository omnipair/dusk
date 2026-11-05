#!/usr/bin/env bash
#
# Upload the IDL of the build that was just deployed.
#
# publish-sdk.yaml publishes @omnipair/dusk-sdk on merge only when every IDL
# the package ships matches its program's IDL account on the cluster, and
# `solana program deploy` leaves that account as it was. The deploy scripts
# run this right after deploying, so the account describes the program that
# runs. The program is the one the IDL names in its `address`.
#
#   scripts/v2/upload_idl.sh target/idl/<program>.json
set -euo pipefail

IDL_FILE="${1:?usage: scripts/v2/upload_idl.sh target/idl/<program>.json}"
if [ ! -f "$IDL_FILE" ]; then
  echo "$IDL_FILE does not exist. Build the program first, for example with yarn v2:build-devnet." >&2
  exit 1
fi
CLUSTER="${ANCHOR_PROVIDER_URL:-https://api.devnet.solana.com}"
WALLET="${ANCHOR_WALLET:-deployer-keypair.json}"
PROGRAM_ID="$(node -p 'require(require("node:path").resolve(process.argv[1])).address' "$IDL_FILE")"
ARGS=(--provider.cluster "$CLUSTER" --provider.wallet "$WALLET")

# Without target/deploy, the first Anchor command rewrites the program IDs
# declared in the source.
node scripts/prepare_build.js

ERRORS="$(mktemp)"
trap 'rm -f "$ERRORS"' EXIT
if AUTHORITY="$(anchor idl authority "${ARGS[@]}" "$PROGRAM_ID" 2> "$ERRORS")"; then
  PAYER="$(solana-keygen pubkey "$WALLET")"
  if [ "$AUTHORITY" != "$PAYER" ]; then
    echo "The IDL account of $PROGRAM_ID belongs to $AUTHORITY, not to $PAYER ($WALLET)." >&2
    exit 1
  fi
  anchor idl upgrade "${ARGS[@]}" --filepath "$IDL_FILE" "$PROGRAM_ID"
elif grep -q AccountNotFound "$ERRORS"; then
  # A program deployed to a new ID has no IDL account yet.
  anchor idl init "${ARGS[@]}" --filepath "$IDL_FILE" "$PROGRAM_ID"
else
  cat "$ERRORS" >&2
  echo "Could not read the IDL authority of $PROGRAM_ID from $CLUSTER." >&2
  exit 1
fi
