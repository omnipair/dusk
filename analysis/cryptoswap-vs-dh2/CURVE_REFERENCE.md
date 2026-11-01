# Clean-room Curve native reference boundary

The native comparison is pinned to **Twocrypto-NG v2.1.0**, commit
`5cbe558902402e8fcb331463089db65fc56c11f9`. The benchmark is an independent
analytical reference. It does not copy or vendor Curve source code, tests,
constants, comments, or implementation structure. Curve's repository license
remains restrictive; this reference is for comparative analysis, not a Dusk
implementation template.

## Independently expressed model

For normalized reserves `x` and `y`, invariant `D`, balance measure
`q = 4xy/D^2`, paper amplification `A`, and concentration parameter `gamma`,
the benchmark solves the public CryptoSwap relationship in the form

```text
K = A q gamma^2 / (gamma + 1 - q)^2
K D (x + y - D) + xy - D^2/4 = 0.
```

Physical base reserves are normalized with the stored center before the
invariant is evaluated. After a fee-free exact-input solve, v2.1's imbalance
fee is evaluated at the fee-free endpoint:

```text
balance = 4xy / (x + y)^2
weight  = fee_gamma * balance / (fee_gamma * balance + 1 - balance)
fee     = weight * mid_fee + (1 - weight) * out_fee.
```

The comparison enforces the pinned v2.1 parameter domain after converting its
raw `ANN` representation to the paper normalization above: `A` is in
`[0.1, 1,000]` and `gamma` is in `[10^-8, 0.199]`. These are native-valid
bounds, not arbitrary benchmark ceilings. A prior analytical sweep incorrectly
allowed `A` up to 10,000; those out-of-domain matches are invalidated.

The fee is withheld from output and remains in the pool. The internal price
sample from a successful trade becomes the next EMA observation. On the first
trade in a later timestamp, the previous sample is capped above at twice the
stored center and folded into the EMA. The controller never receives the
external benchmark price.

The public `ma_time()` value is treated as an EMA half-life. The pinned v2.1
parameter module stores an internal time constant equal to approximately
`ma_time / ln(2)`; applying `exp(-ln(2) * elapsed / ma_time)` therefore matches
the cached-oracle update. The fixture records both the 601-second exposed
half-life and the 866-second packed time constant so this conversion is
auditable rather than inferred from a label.

For controller accounting, `xcp = D / (2 sqrt(center))`. After a swap, the
new virtual price updates cumulative gross profit. A center move is considered
only when the pre-move profit test passes. The requested movement is the
larger of the configured minimum step and one fifth of the remaining relative
EMA error. The candidate is accepted only if its virtual price remains above
one and its square remains above cumulative gross profit. Recentring changes
the normalization center, not physical token balances.

## Validation fixture

`data/curve_v2_1_validation.json` freezes one Ethereum v2.1 deployment at a
specific block, including parameters, reserves, controller state, and exact
on-chain `get_dy` results in both directions. The float analytical model is
checked within one smallest output unit for the 6-decimal vector and within a
sub-ppb relative tolerance for the 18-decimal vector. This validates the
independent equations and state mapping; it does not claim
instruction-for-instruction or integer-rounding identity with the Vyper
contract.

The replay covers the fixed-parameter swap, EMA, profit accounting, and
profit-gated center controller. V2.1 hard-codes a 50% admin share of the half
of gross xcp profit left after reserving the other half for rebalancing. The
benchmark therefore reports both gross pool return and LP return after an
accrued 25%-of-gross-xcp-profit protocol liability. It does not simulate the
liquidity-operation-triggered, at-most-once-per-day claim hook, token transfers,
liquidity-operation-triggered controller updates, or `A`/`gamma` ramps; the
liability-adjusted value is an economic accrual, not an instruction-level
admin-claim reproduction.

Primary references:

- [CryptoSwap whitepaper](https://docs.curve.finance/assets/files/whitepaper_cryptoswap-de196d8f95aae3164a1226234d6d96ca.pdf)
- [Curve protocol explanation](https://docs.curve.finance/protocol/pool/understanding-cryptoswap)
- [Pinned Twocrypto-NG v2.1.0 pool behavior](https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/contracts/main/Twocrypto.vy)
- [Pinned v2.1 parameter bounds](https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/contracts/main/constants.vy)
- [Pinned repository license](https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/LICENSE)
