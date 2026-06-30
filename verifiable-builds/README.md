# Verifiable Builds

This directory is reserved for generated Omnipair Dusk build artifacts.

Expected generated files:

- `omnipair_v2.so`
- `omnipair_v2.json`
- `omnipair_v2.ts`

## Verify Dusk

```bash
cargo install solana-verify

COMMIT_SHA=<COMMIT_SHA>
RELEASE_TAG=<RELEASE_TAG>

solana-verify verify-from-repo \
  --skip-prompt \
  --base-image solanafoundation/anchor:v0.31.1 \
  --program-id 358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv \
  https://github.com/omnipair/dusk \
  --commit-hash "$COMMIT_SHA" \
  --library-name omnipair_v2 \
  -u mainnet-beta \
  -- --features production \
     --config "env.GIT_REV=\"$COMMIT_SHA\"" \
     --config "env.GIT_RELEASE=\"$RELEASE_TAG\""
```

Release artifacts are produced by the V2-only release workflow.
