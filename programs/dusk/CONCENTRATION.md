# Dusk Concentrated AMM

This document defines Dusk's oracle-less concentrated-liquidity mode. The
implementation is derived independently from the equations below and uses
Dusk-native state, controllers, accounting, and terminology.

## Locked Product Decisions

1. Dusk uses one hybrid invariant, the Dusk Concentrated AMM, with amplified
   depth near an internal center and CPMM tails.
2. `peak_depth = 0` selects the exact CPMM branch; its canonical encoding also
   sets `imbalance_scale = 0`. Concentration is optional per market.
3. Swaps, previews, hLP valuation, leverage, lending risk, and liquidation risk
   use the same applied curve and the same center. There is no hidden CPMM risk
   approximation for a concentrated market.
4. Dusk never reads an external price oracle. Trades move reserves and produce
   the observations that update the internal EMA.
5. AMM shape parameters are timelocked and admitted through a gradual, funded
   ramp. Time alone cannot force an LP-impairing parameter point.
6. Base swap fees and lending interest are claimable and non-compounding. Only
   the dynamic surcharge may temporarily remain in executable reserves as
   protected recentering principal.

## Dusk Concentrated AMM Curve

Reserves are first normalized to common quote-value coordinates at center
price $c$. Let those coordinates be $x$ and $y$, and let $D$ be the
invariant. For readable mathematics, define the real-value protocol parameters

$$
P = \frac{\mathtt{peak\_depth\_nad}}{\mathrm{NAD}},
\qquad
s = \frac{\mathtt{imbalance\_scale\_nad}}{\mathrm{NAD}}.
$$

The balance factor $\rho$, imbalance $\delta$, fade weight $w$, and
effective amplification $\lambda$ are

$$
\begin{aligned}
\rho    &= \frac{4xy}{D^2}, \\
\delta  &= 1-\rho, \\
w       &= \left(\frac{s}{s+\delta}\right)^2, \\
\lambda &= \frac{P}{2}\,\rho w.
\end{aligned}
$$

Inside the concentrated region, $D$ is the root of

$$
\boxed{
\lambda D(x+y-D)+xy-\frac{D^2}{4}=0
}
\qquad\text{(concentrated inner invariant).}
$$

The protocol-fixed shoulder is reached when

$$
\boxed{\delta=s}.
$$

Beyond that shoulder, Dusk follows the exact constant-product level

$$
\boxed{
xy=\frac{D^2}{4}(1-s)
}
\qquad\text{(exact CPMM outer invariant).}
$$

These are the real-value forms of the equations. The runtime evaluates their
NAD-scaled integer equivalents with conservative rounding.

At $x=y$, we have $\rho=1$, $\delta=0$, and $w=1$. Total center
marginal depth is therefore $1+P$ times CPMM depth. As reserve imbalance
grows, both $\rho$ and $w$ reduce the extra depth. At the protocol-fixed
fade knee, $\delta=s$, the squared weight is $w=\tfrac14$. Dusk stops the
concentrated equation there and continues the same invariant level on an exact
CPMM hyperbola. The two branches share the same
reserve point and invariant value, so executable output is continuous. They
are intentionally not tangent: outward flow sees the worse CPMM-side marginal
price, while restoring flow sees the concentrated-side marginal price.

The operator has exactly two invariant controls:

- `peak_depth`: extra marginal depth at the balanced center;
- `imbalance_scale`: how much balance-factor error is tolerated before the
  squared concentration weight reaches the fixed CPMM shoulder.

The squared decay profile, the $\rho$ participation factor, the shoulder rule,
and the exact CPMM continuation are protocol-fixed. There is no third
operator-controlled transition or steepness parameter. The physical shoulder
width depends on both `peak_depth` and `imbalance_scale`. Fee, EMA, and
recenter controls do not alter the invariant.

Endpoint configuration bounds are:

$$
\begin{aligned}
&P=0,\ s=0 &&\text{(exact CPMM)},\\
&2\le P\le 2{,}000,\quad 10^{-7}\le s\le 0.199
&&\text{(concentrated mode)}.
\end{aligned}
$$

NAD-scaled integer ramps may pass through smaller positive `peak_depth` values
when entering or leaving CPMM. Peak depth and imbalance scale interpolate
together; whenever peak depth is positive, the runtime clamps imbalance scale
to at least $10^{-7}$, and both become zero only at the CPMM endpoint.
Operators cannot configure a decay exponent or select another curve profile.
Markets that want less stale off-center depth use a smaller `imbalance_scale`
and recalibrate `peak_depth`.

The integer solver is fail-closed:

- invariant roots carry a certified lower/upper bracket;
- both endpoints are persisted and reused only for an exact reserves, center,
  peak-depth, and imbalance-scale identity match;
- exact-input output uses the conservative upper $D$ endpoint;
- exact-output input rounds upward;
- executable reserve endpoints have bounded error and a safety haircut;
- unresolved marginal-price or tail proofs reject the operation.

Positive-concentration inner states require at least one whole NAD-normalized
common-value unit ($10^9$ common atoms) on each side. Exact CPMM tails bypass
that floor because their marginal price is the explicit raw reserve ratio.
Initialization, partial withdrawal, and every other transition reject an
unsupported inner state; a final public yLP exit may park the permanently
burned `MIN_LIQUIDITY` dust, and a later supported two-sided deposit rebuilds
the exact curve certificate. This is an intentional minimum-useful-depth
constraint, not permission to weaken the proof or trap the final LP.

## Price Movement And Recentring

A trade changes reserves, so it changes the AMM marginal price immediately.
Changing the concentration center does not itself exchange tokens and does not
pretend the reserves already moved to the new external composition.

Dusk maintains:

```text
trade endpoint -> internal price observation -> time-decayed EMA
```

Public quote and event telemetry calls that invariant-preserving endpoint
`end_price_nad` for compatibility. It excludes retained surcharge because that
surcharge is principal funding rather than external traded flow. The separate
`reserve_end_price_nad` is the final executable-reserve marginal price used by
the next quote, hLP safety checks, and risk observation.

At most one center adjustment may be admitted per eligible slot. A candidate
center moves only toward that stored EMA, by at most the configured adjustment
step. If the candidate would consume more protected liquidity than is funded,
the step is halved deterministically; if no permitted step is funded, the
center stays unchanged.

Swaps never admit a center move or parameter-ramp point in the same
instruction. They update reserves, exact price observations, fee state, and
the internal EMA signal, then leave any curve maintenance to the bounded,
permissionless `crank_amm_maintenance` instruction. This split keeps the exact
wide-integer curve proofs below Solana's compute ceiling without introducing a
manager, oracle, or discretionary price input.

The repository's LiteSVM release gate is 1.35 million CU, leaving 50,000 CU
below Solana's 1.4 million transaction ceiling. It gates both the measured
retained-surcharge concentrated high-water path and a valid zero-decimal wide
CPMM path that exercises the exact U512 divergence-fee fallback.

With active hLP supply, hedge correction and curve maintenance use two explicit
permissionless cranks. They are deliberately independent: keepers normally
correct hLP inventory first, but a tiny, low-precision, or cash-constrained hLP
cannot freeze a funded parameter ramp or center move. AMM maintenance
checkpoints actual hLP exposure before and after its one bounded step and leaves
any unexecuted correction pending. New deposits into a target hLP vault are
rejected while that vault carries actionable, cash-constrained, or unhedgeable
exposure; the opposite hLP product is not frozen by it. A due parameter-ramp
point gates both directions so an entrant cannot mint against a pre-ramp NAV
basis. Existing hLP withdrawals are not gated by pending exposure or curve
maintenance. A fully settled vault still uses the ordinary
settlement-divergence guard; a vault with an explicit nonzero partial residual
may exit rather than be trapped behind a reference that the controller cannot
advance. Cash and solvency checks always apply.

Every persisted hLP checkpoint recomputes exposure from actual post-transition
inventory and debt. Residual exposure is recognized as zero only when it is at
most `0.00001` target tokens **and** at most one part per million of current hLP
NAV. Larger residuals—including low-precision token granularity—remain pending
and are never relabeled as harmless rounding. Normally they pause new hLP entry
until another hedge crank, ordinary state change, or an hLP exit makes a
smaller correction executable.

There is one narrow admission exception for a production-controller endpoint:
the uncapped proportional hedge plan must prove that integer raw-token or yLP
rounding cannot express one complete correction. A top-up is then accepted
only if the post-deposit residual is still settled or controller-granularity
limited, keeps its sign unless it reaches zero, does not grow in absolute
value, and does not grow per unit of NAV or hLP supply. The signed residual
remains stored and its settlement reference does not advance. An actionable,
cash-constrained, zero-NAV, or unhedgeable vault cannot use this exception.
Pending exposure is not itself an exit gate and never blocks global curve
maintenance.

If integer share accounting rounds an active vault's target-side claim to zero,
no finite proportional hedge adjustment exists. Dusk records the signed
opposite exposure as a fail-closed pending signal and performs no no-op reserve
mutation. An underwater vault is likewise recorded with zero NAV and nonzero
pending exposure. Active zero-NAV vaults reject new hLP entry even when their
opposite exposure is exactly neutral. These vault-local states do not make
ordinary market updates or AMM maintenance fail.

Decoupling controller progress from hedge execution is a liveness tradeoff:
unresolved hLP exposure can persist across bounded funded center or parameter
steps. It remains explicit in `pending_rebalance`; the hLP never receives an
implicit favorable mark merely because maintenance ran. The cached settlement
reference advances only after pending exposure reaches zero, so a partial or
no-op crank—and a granularity-limited top-up—cannot ratchet the allowed
divergence band.

Recentering changes future quotes: it moves the high-depth region relative to
the unchanged reserves. Arbitrage and ordinary flow then determine subsequent
reserve composition. This is the source of recentering cost; Dusk does not hide
it as a bookkeeping-only operation.

The exact CPMM tail creates one useful exception. While the current reserves
and the candidate center both classify the pool on the same outer branch, raw
CPMM quotes and balanced-equivalent $Q$ are independent of the center. Dusk may
therefore move the concentration/fee anchor through that tail with zero curve
impairment. The first step that would bring the reserves back inside the
concentrated shoulder is not free: it is solved and admitted through the same
protected-profit gate. This gives Dusk a protocol-native lazy recentering zone
without pretending that entry back into concentrated liquidity has no cost.

For CPMM, the center is not part of the trading invariant. It remains an
internal divergence-fee anchor and may move without claiming a fictitious
curve impairment.

## Protected Recentring Budget

Dusk Concentrated AMM exposes balanced-equivalent liquidity:

$$
Q^2=\frac{D^2\,\mathrm{NAD}}{4c},
\qquad
q_{\mathrm{LP}}=\frac{Q}{S_{\mathrm{eligible}}},
$$

where $c$ is the center price and $S_{\mathrm{eligible}}$ is eligible yLP
supply. The subscript distinguishes per-share liquidity $q_{\mathrm{LP}}$
from the balance factor $\rho$ used in the invariant.

The AMM stores a protected per-share floor. The spendable budget is the excess
of current $q_{\mathrm{LP}}$ over that floor.

Only a retained dynamic surcharge may lower the economic gap between current
$q_{\mathrm{LP}}$ and the floor. These transitions are neutral and cannot
manufacture budget:

- lending-interest accrual or realization;
- base swap-fee accrual;
- balanced yLP deposits and withdrawals;
- hLP inventory changes;
- ordinary invariant-preserving swaps;
- donations or unrelated reserve bookkeeping.

A recenter, funded parameter-ramp point, or recognized principal loss consumes
the protected surplus before it may impair ordinary LP principal.

For a fresh target, Dusk values both directions of the next permitted center
step plus any pending parameter-ramp point. The target uses:

- 125% impairment coverage;
- a 1 bp arithmetic guard;
- a 1% of $q_{\mathrm{LP}}$ hard cap;
- 10% hysteresis around the stop threshold.

If the full step exceeds the funded/capped budget, Dusk admits a smaller step
or no step. It never labels an underfunded full adjustment safe.

Retaining surcharge changes executable reserves, so recomputing both
hypothetical center directions after every retained swap would repeat several
expensive Dusk Concentrated AMM proofs. Dusk instead uses a sticky-target
protocol:

1. A retained endpoint marks the prior exact target stale, keeps retention
   armed, and synchronizes the 1% hard cap to current $q_{\mathrm{LP}}$.
2. Once the cached hysteresis stop is funded, one executing swap may route its
   surcharge as claimable. If the target remains stale after that quote,
   retention is immediately re-armed. Preview runs this logic only on a clone.
3. `crank_amm_maintenance` uses the sticky target only as a cheap plausibility
   gate. When it is funded, the crank solves the actual next center or ramp
   candidate against current reserves.
4. A funded candidate executes through the exact impairment gate. An
   underfunded candidate refreshes the target from its exact impairment and
   moves no LP principal. A successful adjustment seeds the next target from
   its exact realized impairment.

The target controls retention and recenter liveness; it never authorizes
spending. Every actual center or parameter candidate is independently solved,
capped, and checked against current protected profit before mutation.
The 1% `retention_hard_cap_nad` caps the protected-liquidity target and the
amount a maintenance step may spend; it is not a cap on the trader's
divergence surcharge. Retention may switch to claimable routing once its
hysteresis condition is met, but the fee equation itself keeps deteriorating
with outward flow.

## Fee Model

Every swap has three fee components:

$$
\boxed{
\begin{aligned}
f_{\mathrm{total}}
&=f_{\mathrm{base}} \\
&\quad+f_{\mathrm{divergence}} \\
&\quad+f_{\mathrm{volatility}}
\end{aligned}
}
$$

There is no configured maximum divergence-fee rate. The divergence marginal
toll is unbounded, while the volatility mapping is asymptotic and remains
strictly below 100% at every finite state. The implicit gross-input solve
always requires at least one raw input quantum that is positive after curve
normalization. Launch markets accept asset decimals from zero through nine;
market initialization rejects finer assets. A quote that cannot preserve
positive curve-executable input at supported token precision rejects instead
of silently clipping the toll or accepting a 100% effective charge.

### Divergence surcharge

The divergence component is a state potential over the invariant's input
reserve coordinate:

- flow toward the balanced reserve is free;
- a center-crossing trade pays only for the outward segment;
- already-outward flow pays the increase in one additive potential;
- splitting one monotonic trade cannot materially avoid the charge;
- the marginal toll rises monotonically without a protocol-defined ceiling.

Let $r$ be the input-side reserve in common-value coordinates and $q_0$ its
balanced value. The outward coordinate is

$$
u=\max(r-q_0,0).
$$

For NAD-scaled divergence coefficient $\kappa$, the protocol potential is

$$
\boxed{
F(u)=\frac{4\kappa u^3}
{3\,\mathrm{NAD}\,q_0(q_0+u)}
}.
$$

The surcharge over a path is the difference between endpoint potentials:

$$
\boxed{
f_{\mathrm{divergence}}=F(u_{\mathrm{end}})-F(u_{\mathrm{start}})
}.
$$

Therefore, the exact marginal divergence toll is

$$
\boxed{
F'(u)=
\frac{4\kappa u^2(3q_0+2u)}
{3\,\mathrm{NAD}\,q_0(q_0+u)^2}
}.
$$

Near the center, its marginal rate is quadratic:

$$
F'(u)\approx
4\frac{\kappa}{\mathrm{NAD}}
\left(\frac{u}{q_0}\right)^2.
$$

Far from the center, the marginal toll grows approximately linearly and has no
finite ceiling:

$$
F'(u)\sim
\frac{8\kappa}{3\,\mathrm{NAD}\,q_0}\,u
\xrightarrow[u\to\infty]{}\infty.
$$

The fee-adjusted endpoint is solved against gross input,
so increasing deterioration can make the surcharge share approach 100% but
can never consume the final executable atom. The rational form needs no
iterative square root in the swap hot path.

This targets trending/adverse flow that pushes inventory farther from the
concentrated region.

The configured divergence coefficient is bounded to keep governance and
fixed-point arithmetic inside the audited domain. It scales the potential; it
does not cap its marginal rate or its gross-input share.

### Volatility accumulator

Each successful trade adds a capped symmetric price shock to an accumulator.
The accumulator decays with time and is charged before the current trade's
shock is added. Repeated back-and-forth flow therefore becomes expensive even
when it repeatedly returns toward the center and would pay little divergence
fee.

If $p\ge0$ is the product of decayed volatility and its coefficient, the
protocol maps it to

$$
\nu(p)=\frac{p}{1+p}<1.
$$

The configured coefficient and accumulator caps bound $p$; the mapping itself
is asymptotic and keeps every finite volatility rate below 100%.

This targets chop/mean-reverting toxicity. It is deliberately separate from
the divergence signal because the two signals identify different paths.
At the launch accumulator cap ($10$) and coefficient cap ($100$), the
volatility component approaches

$$
\nu(1000)=\frac{1000}{1001}\approx99.9000999\%.
$$

This fraction applies to the input remaining after the base fee. It is a
consequence of bounded signal pressure, not a configurable fee-rate ceiling.

### Fee destination

Let $I_{\mathrm{credited}}$ be the tokens credited from the trader and
$I_{\mathrm{quote}}$ the input that actually reaches the invariant. Then

$$
\begin{aligned}
I_{\mathrm{quote}}
  &=I_{\mathrm{credited}}-f_{\mathrm{base}}-f_{\mathrm{dynamic}},\\
I_{\mathrm{reserve}}
  &=I_{\mathrm{quote}}+f_{\mathrm{retained}},\\
f_{\mathrm{claimable}}
  &=f_{\mathrm{base}}+f_{\mathrm{distributed}}.
\end{aligned}
$$

While protected profit is below target, the whole dynamic surcharge remains as
reserve principal. Once the hysteresis stop target is reached, new dynamic
surcharge becomes 100% yLP-owned claimable yield. The binary gate can overshoot
by at most one swap's surcharge.

Retained surcharge:

- is not a fee liability;
- stays in the reserve vault;
- increases yLP principal value;
- may fund a later recenter or absorb a recognized principal loss;
- is included in the hLP post-swap reserve coordinate.

Base fee and lending interest always stay in their existing non-compounding,
claimable vault/index lanes.

## Lending, Liquidation, Leverage, And hLP

Risk does not use an external oracle or reconstruct a CPMM when the live market
is concentrated.

- Current price and $Q$ come from the exact applied Dusk Concentrated AMM curve.
- Pessimistic underwriting/liquidation prices come from the existing internal
  EMA books.
- At each pessimistic price and conservative $Q$, Dusk reconstructs the
  corresponding Dusk Concentrated AMM reserve shape, then evaluates impact on that same curve.
- The four canonical risk shapes are cached only while their price, Q, center,
  peak-depth, and imbalance-scale inputs remain identical.
- Swap preview accrues interest, advances clock observations, and runs the hLP
  pre-solver on a cloned market. It does not admit a ramp or center move, and
  its account is read-only, so preview cannot persist curve state.
- Leverage trades use the same quote and dynamic-fee engine as spot, so they
  cannot bypass the surcharge.
- hLP pre-positioning distinguishes trader quote input from physical reserve
  credit, and its exact post-correction is valued on the final applied curve.

This satisfies “lending matches swaps” at the **invariant-mechanics** level,
not at the net voluntary-swap-proceeds level. Lending underwriting, health, and
liquidation references currently evaluate the fee-free pessimistic curve in
`math/risk.rs` and `state/market/health.rs`. The auction bidder transfers debt
tokens directly for collateral, so no AMM fee is mechanically owed. Despite its
name, `settle_liquidation_auction_amm` is also externally funded settlement; it
does not sell seized collateral through the AMM.

That boundary needs one explicit release policy because a valid uncapped
dynamic-fee state can make local voluntary AMM exit proceeds far smaller than
the fee-free auction reference:

1. **External-auction model:** retain fee-free risk valuation, rename the
   misleading fallback, and explicitly accept that liquidation liveness depends
   on independent collateral demand and debt-token funding.
2. **Fee-exempt AMM backstop:** implement a bounded liquidation-only conversion
   against the same raw pessimistic curve used by underwriting. Dynamic fees are
   waived or rebated only on this forced path, making modeled and executable
   fallback proceeds agree.
3. **Fee-aware underwriting:** define a protocol-fixed worst-case forced-exit
   fee function, use it for underwriting, health, and liquidation reference,
   and invert the whole monotone net-output function for exact-out. The current
   manipulable volatility accumulator alone must not become the liquidation
   oracle.

Until one policy is selected and tested, the code provides an externally
funded auction, not an AMM-backed liquidation-liveness guarantee.

## Configuration And Upgrade Path

The Dusk Concentrated AMM-ready `Market` account embeds:

- current and applied curve parameters;
- center, price EMA, volatility accumulator, and observation slots;
- protected-liquidity floor, budget, and target;
- active timelocked ramp state;
- exact Dusk Concentrated AMM invariant bracket, observation identity, and risk-shape cache;
- sticky-target state used by the retained-surcharge compute path;
- reserved extension bytes.

Because this layout lands before public deployment, a later contract upgrade
can enable or tune concentration for an existing market without resizing or
migrating that market account. Pre-launch development accounts using an older
layout must be recreated.

The two serialized curve fields are `peak_depth_nad` and
`imbalance_scale_nad`. This layout is pre-launch and intentionally has no
compatibility promise for accounts created by earlier experimental builds;
those development accounts must be recreated.

Governance changes target parameters only. The applied parameters move through
the timelocked, one-funded-step-per-slot ramp:

$$
\mathrm{CPMM}\ \longleftrightarrow\ \mathrm{Concentrated}(P,s),
$$

or between two concentrated profiles,

$$
\mathrm{Concentrated}(P_1,s_1)
\ \longleftrightarrow\
\mathrm{Concentrated}(P_2,s_2).
$$

Peak depth and imbalance scale are one mode switch. CPMM-boundary ramps
interpolate both coordinates proportionally, clamp positive runtime fade to
the protocol minimum, and canonicalize both fields to zero only at the CPMM
endpoint. No half-enabled or numerically unconditioned ramp state can become
executable.

## Known Trade-offs

- Concentration improves near-center execution but makes stale-center flow
  more sensitive; dynamic fees reduce, but cannot eliminate, LVR.
- No external oracle means Dusk learns a new market price only from trades.
  It cannot simultaneously guarantee immediate new-price depth, no loss to
  first informed flow, and no externally capitalized market maker.
- Recentring is not free. The protected budget changes who prepays the
  impairment; it does not abolish the economic cost.
- Retaining surcharge delays its conversion to claimable yield and gives LPs
  principal value that can later be spent on recentering.
- Greater `peak_depth` improves small near-center quotes but increases
  sensitivity to center error and protected-budget demand.
- The CPMM handoff is value-continuous but not tangent. Aggressive
  `peak_depth`/`imbalance_scale` combinations can create a wide one-sided
  marginal-price gap at the shoulder. An external price inside that gap pins
  arbitrage at the shoulder instead of corresponding to a unique reserve
  composition; lending and liquidation therefore use the conservative
  executable side, and production calibration must bound this spread.
- The exact, fail-closed Dusk Concentrated AMM proofs cost more compute than
  CPMM. Swaps and maintenance are separate bounded instructions; SBF compute
  limits and low-notional precision remain explicit release gates.
- Internal EMA manipulation, wash trading, and same-slot path behavior remain
  adversarial-test surfaces even though the design is oracle-less.

## Release Calibration

Do not select production `peak_depth`, `imbalance_scale`, fee coefficients,
half-lives, or adjustment steps from intuition alone. Calibrate them with
replayed trade paths covering:

- one-way jumps and trends;
- fast and slow mean reversion;
- repeated chop;
- center crossing;
- split trades and same-slot bundles;
- thin/low-decimal markets;
- retained-budget depletion and recovery;
- hLP, lending, leverage, and liquidation activity during price movement.

Measure at least trader execution, LP fee income, LVR/markout, protected-budget
use, recenter delay, revert rate, and SBF compute units. CPMM
(`peak_depth = imbalance_scale = 0`) is the mandatory control.
