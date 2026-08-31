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

## The next step

Replay in LiteSVM, which supplies a `Clock`. The harness in
`tests/utils/litesvm-connection.ts` already runs the program; this needs the
deployed binary (`solana program dump <PROGRAM_ID> dusk.so -u devnet`), these
accounts loaded with `setAccount`, the clock set to slot 490764832 and unix
1788151172, and one swap sent. Expect 6047; then instrument the identity and
read the drift.

The drift magnitude is the open question. A missing term would fail every
swap; devnet fails about a quarter on an idle market, which is the signature of
a value hovering at three or four atoms against `MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS = 3`.
