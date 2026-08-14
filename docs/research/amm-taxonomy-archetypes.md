# Research note: AMM taxonomy and archetypes, applied to Dusk

**Source paper:** Daniel Kirste, Niclas Kannengießer, Ricky Lamberty, Ali Sunyaev,
*Automated Market Makers in Cryptoeconomic Systems: A Taxonomy and Archetypes*.
arXiv:2309.12818 (v3), published in ACM Computing Surveys (58:5, art. 113).
<https://arxiv.org/abs/2309.12818> · <https://doi.org/10.1145/3769669>

> **Access note.** `arxiv.org` and every mirror tried (ACM DL, Semantic Scholar API,
> KIT repository, alphaXiv, emergentmind) are blocked by this environment's egress
> proxy. The framework summarized in §1 is reconstructed from the abstract and
> secondary summaries retrieved via search; the *exact* names of all 21 taxonomy
> dimensions and 53 characteristics were not recoverable and are therefore not
> reproduced here. Everything in §2 onward is analysis of the Dusk source tree at
> `0603d71` and does not depend on the unrecovered detail. Anyone with library
> access should diff §1 against the real Table 2 before treating it as canonical.

---

## 1. What the paper provides

It is a **survey and design-vocabulary paper**, not a new mechanism. It is built
from ~122 scientific publications and ~110 real-world AMMs (Uniswap v2, Curve,
DODO, bonding-curve systems, oracle AMMs). Two artifacts come out of it.

### 1.1 A problem decomposition

The paper frames AMM design around the **thin market problem (TMP)**: when
liquidity is low, quoted prices stop being reliable, which propagates into every
system that consumes those prices. TMP is decomposed into two sub-problems that
must *both* be solved:

| Problem | Definition |
| --- | --- |
| **LAP** — liquidity accumulation problem | Accumulating enough token reserves to settle trades at all. |
| **PDP** — price determination problem | Incorporating enough market information that the quoted price approaches the efficient price. |

The load-bearing claim: solving one does not solve the other, and an AMM that
solves only PDP still fails under TMP. This is the part of the paper that is
directly useful to Dusk.

### 1.2 A taxonomy and three archetypes

21 dimensions / 53 characteristics grouped into four families: **governance,
liquidity, pricing, trading**.

Two dimensions do the archetype separation — **token price source** and **source
of liquidity** — because they dominate how a design attacks LAP and PDP:

| Archetype | Price source | Liquidity source | Characteristic exposure |
| --- | --- | --- | --- |
| **Price-discovering, LP-based** | Internal invariant | LP deposits | Depends on arbitrage for correctness; LPs bear IL/LVR. (CPMM/CFMM: Uniswap v2, Curve) |
| **Price-adopting, LP-based** | External feed | LP deposits | Arbitrage and IL suppressed; inherits the oracle's trust and liveness assumptions. |
| **Price-discovering, supply-sovereign** | Internal invariant | Mint/burn against a curve | Solves LAP by construction at issuance time; used for bootstrap/issuance. (Bonding curves) |

A fourth cell (price-adopting supply-sovereign) is logically possible but had no
real-world instance among the 110 AMMs surveyed, and was dropped.

---

## 2. Where Dusk lands

**Dusk is squarely archetype 1: a price-discovering, LP-based AMM** — with a
lending market bolted onto the same reserves.

| Dimension | Dusk | Code |
| --- | --- | --- |
| Token price source | Internal. `P = R_live[quote] / R_live[base]` | `math/gamm.rs::market_spot_price_nad` |
| Source of liquidity | LP deposits: two-sided `yLP`, single-sided `hLP` | `instructions/liquidity/` |
| Invariant | Constant product over *live* reserves | `math/gamm.rs::calculate_normalized_amount_out` |
| Fee | Static per market, 30 bps default | `config.swap_fee_bps` |
| Governance | Futarchy authority + operator/manager split + 24 h timelock | `state/futarchy_authority.rs`, `MARKET_GOVERNANCE_DELAY_SLOTS` |
| Risk pricing | Symmetric EMA + downward-ratcheting directional EMA, pessimistic virtual reserves | `math/risk.rs`, `state/market/risk.rs` |

The consequential detail: **`live_reserve` is not cash.**

```rust
// state/market/reserves.rs
// live_reserve = cash_reserve + cash_backed_debt + hlp_live
```

Price, depth, collateral valuation, and every circuit breaker are computed from
`live_reserve`. Cash is what actually leaves the vaults.

---

## 3. Learnings, ranked by what they'd change

### 3.1 Dusk doesn't just *quote* at a price-discovered price — it *underwrites credit* at one

In a plain archetype-1 AMM, PDP failure costs traders slippage and LPs adverse
selection. Both are bounded by the trade. In Dusk the same number flows
`market_spot_price_nad` → EMA books → `collateral_value_nad` → `health_bps` →
liquidation trigger. **PDP is a solvency input, not a UX input.**

The paper's framing makes the asymmetry explicit: Dusk carries archetype-1's
price-quality exposure at archetype-1 severity on the swap path, and at
lending-protocol severity on the credit path, from one shared signal. That is the
central design fact about Dusk and it's worth stating that plainly in the
protocol docs, because it's what a reviewer will reach for first.

### 3.2 The mitigations are all PDP-side; TMP says the binding constraint is LAP-side

Dusk's defensive stack is genuinely strong, and all of it is *signal filtering*:

- symmetric EMA + directional (min-ratcheting) EMA — `math/risk.rs::directional_ema_u64`
- pessimistic virtual reserves at `min(P_ema, P_directional)` — `construct_normalized_virtual_reserves_at_pessimistic_price`
- spot/EMA divergence breaker — `assert_price_divergence`
- k-EMA drawdown breaker — `assert_k_drawdown`
- cached pre-transaction observations, so same-instruction spot can't feed the book

Filtering a bad signal yields a *slower* bad signal, not a good one. Against a
genuinely thin market the divergence breaker's success mode is halting the
market — which is correct, but is failure-handling, not a solution.

**What's missing is a stock limit on credit as a function of absolute depth.**
Dusk already flow-limits on EMA'd depth:

```rust
// math/risk.rs
daily_limit_from_liquidity_ema(liquidity_ema, asset_decimals, limit_bps)
```

but `max_daily_borrow_bps` bounds *velocity*, not the outstanding total. Nothing
ties `D_total[i]` to `liquidity_ema` / `k_ema`. `MIN_LIQUIDITY = 1_000` is a sqrt
precision floor, not a risk floor. `recognized_collateral_cap_bps` caps recognized
collateral *relative to that position's debt* — it doesn't know how deep the
market is.

**Consider:** a depth-conditioned cap on outstanding debt per side, reusing the
`liquidity_ema` already in `Risk`. The state is stored; only the constraint is
absent.

### 3.3 The strongest concrete finding: depth conservatism is `min(live, EMA(live))`, never referenced to cash

Credit is right to be more careful about depth than the swap path is, and Dusk
is. `conservative_risk_reserve_depth` (`state/market/health.rs`) uses:

```rust
side.reserves.live_reserve.min(liquidity_ema)
```

That defends against *sudden* depth inflation. It does **not** defend against
*sustained* synthetic depth, for a structural reason: both terms derive from
`live_reserve`, which includes `R_hLP_live`. Hold inflated hLP depth for a few
half-lives (`ema_half_life_ms` bounds are 60 s … 12 h) and the EMA converges up
to it. The `min()` stops being binding, and collateral is then valued against
depth that is not withdrawable cash — while the liquidation path that has to
realize that collateral is constrained by cash.

Dusk's own README already names the seam: spot-neutral hLP rebalancing
"preserves spot, but not depth." The risk book instruments depth four ways
(`liquidity_ema`, `base_liquidity_ema`, `quote_liquidity_ema`, `k_ema`) and every
one is computed from live reserves. There is no cash-referenced quantity anywhere
in collateral valuation.

**Consider:** carry a cash-backing ratio `R_cash[i] / R_live[i]` in `Risk`, EMA it
on the same cadence, and haircut recognized collateral by it. This is the single
highest-value idea the taxonomy frame surfaces for Dusk, and it fits the existing
`Risk::refreshed` structure without new accounts or new instructions.

Note that `assert_k_drawdown` is deliberately one-sided (`if current_k >= k_ema
{ return Ok(()) }`). For a pure AMM that's correct — more depth is never a risk.
For an AMM whose depth is partly synthetic and whose depth feeds credit, depth
*inflation* is a risk direction, and it's currently unguarded.

### 3.4 "Oracle-less" is imprecise: hLP settlement is locally price-adopting

Under the taxonomy, yLP is price-discovering. hLP is not: NAV settles against a
**cached settlement reference** with a divergence guard
(`settlement_divergence_bps`, `emergency_exit_haircut_bps`). That is
price-*adopting* behavior.

The paper's point about price-adopting designs is that they inherit the trust and
liveness assumptions of the feed they adopt. Dusk adopts *its own* cached EMA — so
the manipulation surface isn't removed, it's made recursive: move depth → move the
EMA → move the settlement reference. The guards are the right guards. The framing
should be **"Dusk uses an internal oracle derived from its own reserves,"** not
"Dusk is oracle-less." Reviewers will make this exact point; making it first is
cheaper.

### 3.5 The archetype Dusk doesn't use is the one built for the phase Dusk is weakest in

Archetype 3 (supply-sovereign / bonding curve) exists specifically to solve LAP
during **issuance**, when LP-based liquidity doesn't exist yet. Dusk markets are
at their thinnest on day one — precisely when PDP is worst and when the credit
machinery is most dangerous.

Dusk's only bootstrap gate today is a timestamp:

```rust
// state/market/mod.rs
require!(now >= self.config.start_time, ErrorCode::MarketNotStarted);
```

A timestamp doesn't know how deep the market is. And the EMA books seed from the
first observation:

```rust
// math/risk.rs
pub(crate) fn ema_u64(last_ema: u64, input: u64, ...) -> u64 {
    if last_ema == 0 || input == 0 { return input; }
    ...
}
```

so at seeding `ema == spot`, `assert_price_divergence` compares a value to itself
and passes vacuously, and `liquidity_ema` equals whatever liquidity was just
deposited — which is also the input to the daily borrow limit. A market can be
borrowed against before its risk book has any history to be conservative *with*.

**Consider:** a staged market lifecycle, gating on state rather than time —
*bootstrap* (swap only; no borrow, hLP, or leverage) → *lending enabled* once
`liquidity_ema` clears an absolute threshold **and** the book has ≥ N half-lives
of observations → *full*. The ingredients exist (`start_time`, `reduce_only`,
`last_snapshot_slot`); the state machine doesn't. This is the cheapest
high-value change on the list.

### 3.6 Static fee is the LP's only compensation for two distinct risks

`swap_fee_bps` is fixed per market (30 bps default), changeable only through the
24 h timelock. In Dusk, LP inventory backs swaps *and* underwrites credit, so
that one number compensates LVR/IL **and** credit risk — and the taxonomy treats
fee/slippage as a dimension to be matched to asset correlation and volatility.

Dusk already computes the raw material for a volatility-linked component: the gap
between the symmetric and directional EMAs is a trend/volatility proxy, and
`k_ema` drawdown is a stress proxy. Not urgent. Worth shaping `MarketConfig` now
so a dynamic component can be added without an account migration.

### 3.7 Governance is a relative strength; the gap is parameter tiering

Against the taxonomy's governance family Dusk is ahead of the surveyed field:
futarchy authority, 24 h timelock, operator/manager separation, per-market and
global reduce-only, Dutch-auction revenue settlement, pending-change accounts with
explicit scheduling.

The gap isn't the mechanism, it's the **risk mapping**. `update_config` moves the
entire `MarketConfig` through one path, so `spot_ema_divergence_bps`,
`k_ema_drawdown_bps`, `recognized_collateral_cap_bps`, `market_health_min_bps`
and the three EMA half-lives — all solvency parameters — travel on the same 24 h
delay as `swap_fee_bps` and `manager_fee_bps`.

**Consider:** tier the delay by parameter class, and carve out monotonic safety
moves — *tightening* a breaker should be fast, *loosening* one slow.

---

## 4. What the paper does not help with

Be clear about the ceiling. It is a taxonomy, so it offers no quantitative
guidance on the questions Dusk actually has to answer:

- No LVR/IL treatment deep enough to size `swap_fee_bps` (see Milionis et al. on
  LVR, and the am-AMM auction line of work, for that).
- No model for AMM-as-oracle manipulation cost — the literature that bears
  directly on §3.1–§3.3 is the oracle-manipulation-cost work, not this paper.
- Nothing on integrated lending, leveraged LP vaults, or liquidation design; the
  survey's unit of analysis is the exchange mechanism alone.
- No Solana/CU-cost or account-model considerations.

Its real value to Dusk is **vocabulary and coverage** — a checklist to run the
design against before external review, and a defensible way to state which
archetype Dusk is and which exposures come with that choice.

---

## 5. Suggested follow-ups

Ordered by value per unit of work:

1. **Cash-backed depth ratio in the risk book** (§3.3) — add `R_cash/R_live`,
   EMA it, haircut recognized collateral by it. Fits `Risk::refreshed`.
2. **State-gated market lifecycle** (§3.5) — replace the `start_time`-only gate
   with a depth + book-maturity gate before lending/hLP/leverage unlock.
3. **Depth-conditioned stock cap on debt** (§3.2) — the flow limit already exists;
   add the stock limit on the same `liquidity_ema`.
4. **Tiered governance delays** (§3.7) — split solvency parameters from economic
   parameters; fast-path monotonic tightening.
5. **Docs precision** (§3.4) — describe Dusk as using an internal reserve-derived
   oracle, and state the archetype-1 exposure explicitly.
6. **Config surface for a dynamic fee** (§3.6) — shape now, implement later.

Items 1–3 are the ones that change Dusk's risk profile. Items 4–6 are hygiene.
