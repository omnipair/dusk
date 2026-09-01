# Verifiable Builds

This directory is reserved for generated Omnipair V2 (Dusk) build artifacts.

Expected generated files:

- `dusk.so`
- `dusk.json`
- `dusk.ts`

## Verify Dusk

```bash
cargo install solana-verify

COMMIT_SHA=<COMMIT_SHA>
RELEASE_TAG=<RELEASE_TAG>

solana-verify verify-from-repo \
  --skip-prompt \
  --base-image solanafoundation/anchor:v0.31.1 \
  --program-id JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X \
  https://github.com/omnipair/dusk \
  --commit-hash "$COMMIT_SHA" \
  --library-name dusk \
  -u mainnet-beta \
  -- --features production \
     --config "env.GIT_REV=\"$COMMIT_SHA\"" \
     --config "env.GIT_RELEASE=\"$RELEASE_TAG\""
```

Release artifacts are produced by the Dusk release workflow.
