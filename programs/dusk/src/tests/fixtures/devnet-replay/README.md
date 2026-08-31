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

The trader's token accounts are now captured too, and signed for by the real
trader — the replay still passes. So that is ruled out as well.

The market capture is the state that failed: `last_update_slot` in it is
490754400 against a failure observed at 490764832, and the field only moves
when a transaction touches the market, so nothing altered it in between.

The market does carry hLP debt for interest to accrue on
(`base_hlp_vault.debt_shares` = 9,935,967), and the clock sweep is effective —
`setClock` demonstrably moves what the program reads. So the sweep really is
sampling accrual, across roughly a day of it, and none of it fails.

**The devnet failure is periodic.** Sampling every 1.5 seconds for a minute
gives `........xxxx........xxxx........xxxx....` — failure clusters beginning
at slots 490774145, 490774335 and 490774522, so a cycle of **187-190 slots**
with roughly a third of it failing. That duty cycle is the 25-33% rate seen
from every sampling method and every RPC provider, so it is a property of the
chain state rather than of which node answered.

A period is what a rounding bug looks like from outside: something accrues
linearly, a floored quantity ticks over, the drift crosses the three-atom
tolerance for part of each cycle and falls back inside it for the rest. At
roughly 400ms a slot, ~188 slots is ~75 seconds — the time it takes one more
atom of interest to accrue on this market's hLP debt.

**And it still does not reproduce.** The sweep now walks 401 consecutive
elapsed slots — more than two full cycles — against the deployed binary and
the captured state. Every one passes.

Same program, same state, same accounts, every clock offset across two full
periods: passes every time locally, fails a third of the time on devnet. That
is the finding. It puts the trigger outside program
logic and state, in the interaction with the live validator — the remaining
candidates being sysvars other than the clock (rent, epoch schedule, slot
hashes, stake history, all LiteSVM defaults here), or something about how
simulation against a live bank differs from LiteSVM's.

RPC load balancing is ruled out: Helius and `api.devnet.solana.com` both
report 33%, and the periodicity is too regular for node divergence.

`catch_hlp_failure.mjs` closes the last doubt about the capture. It simulates
against devnet until a swap reverts, captures **every** account at that
instant, and replays immediately. Devnet failed at slot 490776326; the same
state, replayed against the deployed binary across 401 clock offsets, passed
every time.

Devnet's epoch state (`epoch` 1136, `epochStartTimestamp` 1788149045) makes no
difference either.

**A warning for the next attempt.** `FeatureSet.allEnabled()` looks like the
obvious next thing to vary, and it reports a 401/401 reproduction. It is not
one: it fails to build the SBF VM at all — `Invalid memory region at index 4` —
so the program never runs. The replay now requires the logs to name
`BrokenInvariant` before counting an attempt as reproduced, because a harness
that cannot start the program otherwise reports the bug it was looking for.

The drift magnitude remains the open question. A missing term would fail every
swap; devnet fails about a quarter on an idle market, which is the signature of
a value hovering at three or four atoms against
`MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS = 3`.
