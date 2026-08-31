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

That leaves one difference, and it is decisive. The replay runs a locally
built binary because **LiteSVM refuses the deployed one**, and the ELF headers
say why:

| | `e_machine` | `e_flags` |
| --- | --- | --- |
| deployed | 247 (`EM_BPF`) | 3 — **SBF version 3** |
| local `cargo-build-sbf` (platform-tools v1.54) | 263 | 0 |

Two different compilation targets. LiteSVM's bundled runtime does not accept
SBFv3, and no loader or feature set changes that — `addProgramWithLoader` with
either loader and `withFeatureSet(allEnabled())` all fail identically. The
dump is authentic: it hashes to `8191b4cf…`, exactly the lock's attested value.

**This is worth knowing beyond the bug.** The deterministic test layer compiles
the program to a different SBF target than devnet runs, so LiteSVM suites
exercise a different artifact from the deployed one. That is a gap in what the
tests can attest to, independent of anything hLP.

**litesvm 1.x loads SBFv3.** The pinned 0.8.0 does not, but 1.4.1 does, so the
replay now runs the deployed artifact itself — no Docker, no rebuild.
`scripts/devnet/replay_hlp_invariant.mjs` does exactly that and documents its
own setup, since upgrading the project's litesvm would also move the smoke
suite and is a separate question.

**And it still succeeds.** Deployed binary, captured failing state, real clock,
and a sweep of accrual from zero to two hundred thousand slots: every point
passes. Binary, state and clock are all faithful, so none of them is the
trigger.

The `--features production` difference is not a factor: it gates only
vanity-suffix checks on LP mint keys at market initialization, nothing on the
swap path.

## Ruled out, with measurements

- The interest subtraction. Removing the second makes `stressed_hlp_recovery`
  fail; guarding it on claim certification drifts a plain swap by 12,499 atoms
  against 12,500 accrued. Both are load-bearing.
- Elapsed slots. Sweeping zero to two hundred thousand slots past the capture
  changes nothing.
- The wall clock. Replaying at the captured block time changes nothing.
- The `production` feature. It touches market initialization only.
- **The binary.** The deployed SBFv3 artifact runs the same swap successfully.

## What that leaves

The replay differs from the chain in three remaining ways, in rough order of
how likely each is to matter:

1. **The trader's token accounts are synthesized**, not captured. They are
   written as a plain 165-byte SPL layout with a balance; devnet's were real
   accounts with their own history.
2. **Sysvars other than the clock** are LiteSVM defaults — rent, epoch
   schedule, slot hashes, stake history.
3. **The captured market may not be the state that failed.** It was read with
   `getAccountInfo` immediately after the reverting simulation rather than
   pinned to that simulation's slot, and RPC cannot read an account at a past
   slot without an archive node. `last_update_slot` in the capture is
   490754400 against a failure at 490764832, so nothing touched the market in
   between — which argues the capture is right, but does not prove it.

The cheapest next experiment is (1): capture the real trader token accounts
alongside the market and replay with those.

The drift magnitude remains the open question. A missing term would fail every
swap; devnet fails about a quarter on an idle market, which is the signature of
a value hovering at three or four atoms against
`MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS = 3`.
