# Captured devnet state, for reproducing the hLP invariant failure

About a quarter of swaps on the devnet market revert with `BrokenInvariant`
(6047) at the hLP reserve identity in
`transitions/liquidity/hlp/engine.rs`. These are the inputs for reproducing it
off chain, so nobody has to instrument a deployed program to make progress.

| File | What it is |
| --- | --- |
| `market_failing.bin` | The market account read at the moment a swap simulation reverted, slot 490764832, block time 1788151172 |
| `market_healthy.bin` | A control, captured while swaps were succeeding |
| `market.bin`, `*_mint.bin` | The other accounts a swap touches |
| `manifest.json` | Addresses, owners and lamports for each |

## What has already been ruled out

**The interest subtraction is not the cause.** The quoted swap path subtracts
the accrued interest tranche twice where the materialized path subtracts once,
which looks wrong and is not: removing the second makes
`stressed_hlp_recovery` fail, and guarding it on claim certification makes a
plain swap drift by a whole interest tranche — 12,499 atoms against 12,500
accrued. Both subtractions are load-bearing.
`a_plain_swap_survives_accrued_hlp_interest` pins this down.

**Replaying the captured state through `prepare` + `finalize_state` does not
reproduce it.** Both captures swap cleanly in a unit test, at the real slot and
block time, across elapsed spans from zero to ten thousand slots.

## Why a unit test cannot close it

The instruction runs `market.update()` before the handler, and `update()` calls
`Clock::get()`, which is unavailable outside the runtime. So the replay above
starts from a market the runtime would never hand the handler: accrual and the
EMA refresh both happen in that step, and it is the most likely place the drift
is introduced.

## The LiteSVM replay, and where it stops

`scripts/devnet/replay_hlp_invariant.ts` does the replay: it loads a program,
sets every account here as captured, sets the clock to slot 490764832 and unix
1788151172, and sends one swap. It works — and the swap **succeeds**, so the
failure does not reproduce.

That leaves one difference, and it is the important one. The replay runs a
locally built binary, because **LiteSVM refuses the deployed one**
(`invalid account data for instruction`). The deployed artifact hashes to
`8191b4cf…`, exactly the lock's attested value, so the dump is authentic; the
local build hashes to `194a1a19…` and is 166KB larger. That gap is the build
environment, not the source: the release process builds inside
`solanafoundation/anchor:v0.31.1` with `--features production`, and that
feature gates only vanity-suffix checks on LP mint keys at market
initialization — nothing on the swap path.

So the replay exercises the same source compiled differently, and the failure
does not appear. Closing this needs one of:

1. **Reproduce the deployed build** with `solana-verify` and its Docker base
   image, then load *that* binary here. Needs Docker.
2. **Instrument and redeploy**, printing the drift at the identity check.

## Ruled out, with measurements

- The interest subtraction. Removing the second makes `stressed_hlp_recovery`
  fail; guarding it on claim certification drifts a plain swap by 12,499 atoms
  against 12,500 accrued. Both are load-bearing.
- Elapsed slots. Sweeping zero to ten thousand slots past the capture changes
  nothing.
- The wall clock. Replaying at the captured block time changes nothing.
- The `production` feature. It touches market initialization only.

The drift magnitude remains the open question. A missing term would fail every
swap; devnet fails about a quarter on an idle market, which is the signature of
a value hovering at three or four atoms against
`MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS = 3`.
