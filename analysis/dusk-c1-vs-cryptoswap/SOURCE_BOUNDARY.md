# Source boundary

This benchmark is a clean-room analytical comparison. It does not copy or
vendor Curve source code, tests, constants, comments, or implementation
structure.

## Dusk source identities

- Repository path: `programs/dusk/CONCENTRATION.md`
  - SHA-256: `5d71f3a553d9aee8254affe017e0064524ded41b18231fba2fbb175bd1af7397`
- Repository path: `programs/dusk/src/math/concentrated.rs`
  - SHA-256: `aa762ee79fdde7a44446cf731225def3d36bc9b50ef8987a74cbf3ce773e9cb6`

These identify the current C0 production checkpoint used as architectural
context. The C1 candidate exists only in this analytical package.

## CryptoSwap reference boundary

- Published whitepaper:
  <https://docs.curve.finance/assets/files/whitepaper_cryptoswap-de196d8f95aae3164a1226234d6d96ca.pdf>
- Protocol explanation:
  <https://docs.curve.finance/protocol/pool/understanding-cryptoswap>
- Pinned restrictive source license:
  <https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/LICENSE>
- Observable fixture identity:
  `a60352ec92bf0c96a0b55c344cacae936ab586015ffd701b9b8d79a30f017ff9`

The analytical CryptoSwap equations in `models.py` were independently derived
from the published mathematics. The restrictive source license remains an
explicit limit on source reuse.
