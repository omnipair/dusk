#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

RPC_PORT="${SURFPOOL_RPC_PORT:-8899}"
WS_PORT="${SURFPOOL_WS_PORT:-8900}"
HOST="${SURFPOOL_HOST:-0.0.0.0}"
NETWORK="${SURFPOOL_NETWORK:-mainnet}"
LOG_PATH="${SURFPOOL_LOG_PATH:-/tmp/dusk-surfpool-logs}"
WALLET_PATH="${ANCHOR_WALLET:-deployer-keypair.json}"
PROGRAM_ID="${DUSK_PROGRAM_ID:-358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv}"
LEVERAGE_DELEGATE_PROGRAM_ID="${DUSK_LEVERAGE_DELEGATE_PROGRAM_ID:-EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp}"
DEPLOYMENT_TIMEOUT_SECONDS="${FORK_LAB_DEPLOYMENT_TIMEOUT_SECONDS:-180}"

if [[ "$RPC_PORT" != "8899" && "${FORK_LAB_ALLOW_NONSTANDARD_SURFPOOL_PORT:-false}" != "true" ]]; then
  cat >&2 <<EOF
Surfpool's generated Anchor deployment runbook currently targets http://127.0.0.1:8899.
Refusing to start the fork on port $RPC_PORT because local program upgrades can be skipped.

Use the default 8899 for the private v2-surfpool-rpc service. The API/proxy services can still
use Railway PORT. If you intentionally do not want local program deployment, set
FORK_LAB_ALLOW_NONSTANDARD_SURFPOOL_PORT=true.
EOF
  exit 1
fi

if [[ -n "${FORK_LAB_PAYER_KEYPAIR_JSON:-}" || -n "${FORK_LAB_PAYER_KEYPAIR_BASE64:-}" ]]; then
  node scripts/v2-fork-lab/materialize_fork_signer.mjs "$WALLET_PATH"
elif [[ "${DUSK_REQUIRE_EXPLICIT_FORK_SIGNER:-false}" == "true" ]]; then
  echo "Hosted Surfpool requires the same explicit FORK_LAB_PAYER_KEYPAIR_JSON or FORK_LAB_PAYER_KEYPAIR_BASE64 on RPC and API services." >&2
  exit 1
elif [[ ! -f "$WALLET_PATH" ]]; then
  mkdir -p "$(dirname "$WALLET_PATH")"
  node -e "const { Keypair } = require('@solana/web3.js'); const fs = require('fs'); fs.writeFileSync(process.argv[1], JSON.stringify(Array.from(Keypair.generate().secretKey)))" "$WALLET_PATH"
fi

if [[ "${FORK_LAB_BUILD:-true}" != "false" ]]; then
  anchor build -p dusk -- --features "development"
  anchor build -p leverage_delegate -- --features "development"
fi

for artifact in \
  target/deploy/dusk.so \
  target/deploy/dusk-keypair.json \
  target/deploy/leverage_delegate.so \
  target/deploy/leverage_delegate-keypair.json
do
  if [[ ! -f "$artifact" ]]; then
    echo "Missing required Dusk Surfpool deployment artifact: $artifact" >&2
    echo "Build both Dusk programs with the development feature before starting the fork." >&2
    exit 1
  fi
done

export ANCHOR_WALLET="$WALLET_PATH"
export ANCHOR_PROVIDER_URL="http://127.0.0.1:${RPC_PORT}"

echo "Starting Dusk Surfpool fork on ${HOST}:${RPC_PORT} with local artifact:"
ls -lh target/deploy/dusk.so target/deploy/leverage_delegate.so

BOOT_LOG="$(mktemp -t dusk-surfpool-start.XXXXXX.log)"

cleanup() {
  if [[ -n "${SURFPOOL_PID:-}" ]] && kill -0 "$SURFPOOL_PID" 2>/dev/null; then
    kill "$SURFPOOL_PID" 2>/dev/null || true
    wait "$SURFPOOL_PID" 2>/dev/null || true
  fi
}

trap cleanup INT TERM

surfpool start \
  --network "$NETWORK" \
  --host "$HOST" \
  --port "$RPC_PORT" \
  --ws-port "$WS_PORT" \
  --no-tui \
  --no-studio \
  --yes \
  --legacy-anchor-compatibility \
  --airdrop-keypair-path "$WALLET_PATH" \
  --artifacts-path target/deploy \
  --log-path "$LOG_PATH" > >(tee "$BOOT_LOG") 2>&1 &

SURFPOOL_PID=$!
deadline=$((SECONDS + DEPLOYMENT_TIMEOUT_SECONDS))
dusk_deployed=false
leverage_delegate_deployed=false

while kill -0 "$SURFPOOL_PID" 2>/dev/null; do
  if grep -q "Runbook execution aborted" "$BOOT_LOG"; then
    echo "Surfpool deployment runbook aborted before the local Dusk program was upgraded." >&2
    cleanup
    exit 1
  fi

  if grep -Eq "Program (Created|Upgraded) - Program ${PROGRAM_ID}" "$BOOT_LOG"; then
    dusk_deployed=true
  fi
  if grep -Eq "Program (Created|Upgraded) - Program ${LEVERAGE_DELEGATE_PROGRAM_ID}" "$BOOT_LOG"; then
    leverage_delegate_deployed=true
  fi
  if [[ "$dusk_deployed" == "true" && "$leverage_delegate_deployed" == "true" ]]; then
    if ! node scripts/v2-fork-lab/seed_fork_generation.mjs; then
      echo "Failed to initialize the authoritative Surfpool fork generation marker." >&2
      cleanup
      exit 1
    fi
    if ! DUSK_REQUIRE_EXTERNAL_FORK_MARKER=true \
      TS_NODE_PROJECT=scripts/v2-fork-lab/tsconfig.json \
      node --loader ts-node/esm scripts/v2-fork-lab/bootstrap_fork.ts; then
      echo "Failed to bootstrap Dusk markets from the single Surfpool RPC controller." >&2
      cleanup
      exit 1
    fi
    echo "Surfpool fork is running local Dusk artifacts for ${PROGRAM_ID} and ${LEVERAGE_DELEGATE_PROGRAM_ID}."
    wait "$SURFPOOL_PID"
    exit $?
  fi

  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for Surfpool to deploy local Dusk program artifact." >&2
    echo "Expected deploy logs for ${PROGRAM_ID} and ${LEVERAGE_DELEGATE_PROGRAM_ID}." >&2
    cleanup
    exit 1
  fi

  sleep 1
done

wait "$SURFPOOL_PID"
