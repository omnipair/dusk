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

## CLOSED — the drift is 24-26 atoms, and interest has nothing to do with it

Measured on chain 2026-08-31 by deploying a build with `--features
debug-hlp-drift`, reading the logs, and restoring the attested binary
immediately afterwards. Twenty-five consecutive swaps, every one reverting:

```
hlp-drift base=1 quote=25 base_interest=0 quote_interest=0 final_base_debt=1482619 final_quote_debt=0
```

**Both interest tranches are zero.** The quote-side drift is 24-26 atoms
against a three-atom tolerance, with no accrued interest anywhere in the
transition. Every explanation built around accrual — including the one this
file argued at length — was wrong.

**And the rates quoted throughout this file were wrong too.** They counted any
simulation error as a revert, and `BlockhashNotFound` — the RPC declining to
simulate, with the program never running — is both common and periodic. That
is where the "quarter of swaps on an idle market" and the ~188-slot cycle came
from. Re-measured with program reverts only: **0 of 12 on an idle market, 7 of
12 with 400 quote borrowed.** Debt brings the failure on; an idle market is
fine.

**The three-atom tolerance is arithmetically correct, and that is the point.**
`NAD_DECIMALS` is 9 and the assets carry 6 decimals, so
`denormalize_from_nad_floor` divides by 1000 and discards at most 999 NAD —
under one atom per call. Three floored quantities therefore genuinely cannot
exceed three atoms, exactly as the comment claims.

So a 25-atom drift is **not a rounding problem and the constant is not too
tight**. The two sides disagree about real value. What is actually disagreeing
is:

```
(quote_live_reserve - old_quote_hlp_live)      // the materialized reserves
vs
(ordinary_quote + quote_equity)                 // the quoted endpoint, rebuilt
```

Both `ordinary_quote` and `quote_equity` come from
`denormalize_from_nad_floor` on the quoted endpoint; the stored
`quote_hlp_live_reserve` is a raw atom count maintained incrementally. Twenty
five atoms is far outside what those floors can produce, so the quoted
endpoint's model of the post-swap state and the materialized state differ by
real value — an accounting disagreement, not a precision one.

A confirming reading from a *passing* swap, where the model checks out exactly:

```
hlp-terms base_live=25148093165 quote_live=24947210813 old_base_hlp=0
          old_quote_hlp=9922204 final_base_live=25148093164 final_quote_live=24947206876
```

`24947210813 - 9922204 + 9918268 = 24947206877` against a final of
`24947206876` — one atom, which is what the floors should cost.

The asymmetry is the strongest clue left: base drifts by 1 and quote by 25,
and only the base hLP vault carries debt (`final_base_debt` 1,482,619,
`final_quote_debt` 0). The side *without* debt is the side that drifts.

Widening the constant would not be a fix, it would be suppressing a real
discrepancy: the identity is doing its job by rejecting. What needs deciding is
why the quoted endpoint and the materialized reserves disagree about value once
hLP debt is outstanding, which is a question about the AMM's accounting model
rather than about tolerances.

## What was ruled out along the way

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

## The instrumented build

The remaining way to learn anything is to read the drift on chain, and the
build that prints it is ready behind a feature flag:

```bash
anchor build -p dusk -- --features debug-hlp-drift
# deploy, then simulate a swap and read the logs for "hlp-drift"
```

It logs `base`, `quote`, both interest tranches and both final debts at the
identity check, then decides exactly as before. Off by default, and it changes
nothing when off — the full suite passes identically with and without it.

It is a diagnostic, not a fix. It says what the drift is; it does not say
whether three atoms is the right bound. Do not ship it enabled: it puts a log
line on the hot path of every swap.

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
