#!/usr/bin/env bash
#
# Deploy one program to devnet, then upload its IDL from the same build.
#
# The program keypair decides which program `solana program deploy` writes,
# while the `address` in the IDL decides which IDL account upload_idl.sh writes
# and which program the SDK calls. The script stops before deploying when the
# two name different programs, so the code and the IDL that publish-sdk.yaml
# checks always describe the same program.
#
#   scripts/v2/deploy_program.sh target/deploy/<program>.so <program-keypair> target/idl/<program>.json
set -euo pipefail

USAGE="usage: scripts/v2/deploy_program.sh target/deploy/<program>.so <program-keypair> target/idl/<program>.json"
PROGRAM_SO="${1:?$USAGE}"
PROGRAM_KEYPAIR="${2:?$USAGE}"
IDL_FILE="${3:?$USAGE}"
WALLET="${ANCHOR_WALLET:-deployer-keypair.json}"

if [ ! -f "$PROGRAM_KEYPAIR" ]; then
  echo "$PROGRAM_KEYPAIR does not exist." >&2
  exit 1
fi
for FILE in "$PROGRAM_SO" "$IDL_FILE"; do
  if [ ! -f "$FILE" ]; then
    echo "$FILE does not exist. Build the program first, for example with yarn v2:build-devnet." >&2
    exit 1
  fi
done

PROGRAM_ID="$(node -p 'require(require("node:path").resolve(process.argv[1])).address' "$IDL_FILE")"
KEYPAIR_ID="$(solana-keygen pubkey "$PROGRAM_KEYPAIR")"
if [ "$KEYPAIR_ID" != "$PROGRAM_ID" ]; then
  echo "$PROGRAM_KEYPAIR is the keypair of $KEYPAIR_ID, but $IDL_FILE was built for $PROGRAM_ID. Nothing was deployed; pass the keypair of $PROGRAM_ID." >&2
  exit 1
fi

solana program deploy "$PROGRAM_SO" --program-id "$PROGRAM_KEYPAIR" --keypair "$WALLET" --upgrade-authority "$WALLET" --url devnet
scripts/v2/upload_idl.sh "$IDL_FILE"
