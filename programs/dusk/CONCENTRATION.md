# Dusk Concentrated AMM

This document defines Dusk's oracle-less concentrated-liquidity mode. The
implementation is derived independently from the equations below and uses
Dusk-native state, controllers, accounting, and terminology.

## Locked Product Decisions

1. Dusk uses one hybrid invariant, the Dusk Concentrated AMM, with amplified
   depth near an internal center and CPMM tails.
2. `peak_depth = 0` selects the exact CPMM branch; its canonical encoding also
   sets `fade_scale = 0`. Concentration is optional per market.
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

For positive concentration, reserves are first normalized into an adaptive
common numeraire at center price $c$. If $B$ and $Q$ are the raw
NAD-normalized base and quote reserves, then

$$
(x,y)=
\begin{cases}
\left(B\dfrac{c}{\mathrm{NAD}},\ Q\right), & c\ge \mathrm{NAD}
\quad\text{(quote numeraire)},\\[6pt]
\left(B,\ Q\dfrac{\mathrm{NAD}}{c}\right), & c<\mathrm{NAD}
\quad\text{(base numeraire)}.
\end{cases}
$$

Runtime applies the indicated conversions with explicit floor/ceiling rules.
The higher-valued asset is always converted into the lower-valued asset's
unit, so every raw normalized input atom advances by at least one common atom;
there is no low-center dead input bucket. Let $D$ be the invariant in that
common numeraire. For readable mathematics, define the real-value protocol parameters

$$
P = \frac{\mathtt{peak\_depth\_nad}}{\mathrm{NAD}},
\qquad
s = \frac{\mathtt{fade\_scale\_nad}}{\mathrm{NAD}}.
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

The inner equation runs until the protocol-fixed transition start

$$
\boxed{\delta_0=\frac{s}{4}},
\qquad
\rho_0=1-\frac{s}{4}.
$$

The outer constant-product level is

$$
\boxed{
xy=\frac{D^2}{4}(1-s)
}
\qquad\text{(exact CPMM outer invariant).}
$$

The protocol joins those two regions with one finite, protocol-defined
transition. Define the symmetric reserve-imbalance coordinate

$$
r=\frac{\max(x,y)}{\min(x,y)},
\qquad
v=\frac12\left(\sqrt r-\frac1{\sqrt r}\right).
$$

Let $v_0$ be the value of $v$ where the inner root reaches $\rho_0$, and let

$$
m_0=-\left.\frac{d\rho_{\mathrm{inner}}}{dv}\right|_{v=v_0}>0,
\qquad
\rho_t=1-s.
$$

The transition length and normalized progress are fixed by

$$
L=\frac{3(\rho_0-\rho_t)}{m_0},
\qquad
z=\frac{v-v_0}{L}.
$$

For $0\le z<1$, the transition target is

$$
\boxed{
\rho_{\mathrm{transition}}(v)
=
\rho_t+(\rho_0-\rho_t)(1-z)^3
}.
$$

At $z\ge1$, $\rho=\rho_t$ exactly and the invariant is the CPMM level above.
The cubic has the inner derivative $-m_0$ at the first join and zero
derivative at the CPMM join. Consequently reserve level and marginal price
are continuous at both joins: the complete curve is finite and $C^1$; its
second derivative need not be continuous.

These are the real-value forms of the equations. Runtime derives the
authoritative finite-$C^1$ geometry in Q80 only when the applied shape
parameters change. Ordinary quotes consume its cached Q64 projection:
transition targets and residuals are evaluated in Q64, and the Q80 sign path
is invoked only when the coarse residual is within eight Q64 ulps of zero.
Inner and exact-tail branch classification uses a low/high reserve-ratio
threshold and does not take a software square root; only a transition probe
needs the Q64 radial coordinate. Marginal-price and risk-shape projections use
Q48. Q80 is a fixed-point precision, not a second invariant or a runtime
big-integer type. `s/4`, the cubic profile, and the derivative-matched length
are protocol constants—not market configuration.

The persisted geometry cache binds the math revision and the applied
`peak_depth_nad`/`fade_scale_nad` pair. It stores the authoritative Q80 peak,
scale, two radial joins, two reserve-ratio joins, and starting slope. Its Q64
and Q48 projections are exact right shifts. Cache fields are private derived
state: only the curve math constructs them. A parameter ramp derives its
candidate cache locally and commits parameters, geometry, and revision
atomically after funding succeeds. Center and reserve changes reuse the cache;
CPMM mode keeps it empty.

At $x=y$, we have $\rho=1$, $\delta=0$, and $w=1$. Total center
marginal depth is therefore $1+P$ times CPMM depth. As reserve imbalance
grows, both $\rho$ and $w$ reduce the extra depth. Dusk leaves the inner
equation at $\delta=s/4$, crosses the derivative-matched transition, and
reaches an exact CPMM hyperbola at $\rho=1-s$. There is no one-sided boundary
price jump: outward and restoring marginal prices meet at the same value at
each boundary.

The governable concentration family has exactly two invariant coordinates:

- `peak_depth`: extra marginal depth at the balanced center;
- `fade_scale`: the balance-factor span over which center depth fades and the
  exact CPMM tail is reached.

The squared inner decay, the $\rho$ participation factor, the $s/4$ join,
the derivative-matched cubic, and the exact CPMM continuation are
protocol-fixed. There is no third configurable transition or
steepness parameter. The physical transition width depends on both
`peak_depth` and `fade_scale`. Fee, EMA, and
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
when entering or leaving CPMM. Peak depth and fade scale interpolate
together; whenever peak depth is positive, the runtime clamps fade scale
to at least $10^{-7}$, and both become zero only at the CPMM endpoint.
Proposals cannot configure a decay exponent or select another curve profile.
Markets that want less stale off-center depth use a smaller `fade_scale`
and recalibrate `peak_depth`.

The integer solver is fail-closed:

- each solve maintains an ephemeral sign-changing lower/upper bracket;
- reserve solves intersect that bracket with the structural bound $x+y\ge D$,
  then use a safeguarded analytical Newton probe in the finite transition,
  secant probes elsewhere, and deterministic bisection fallback;
- the canonical invariant is the smallest integer $D$ on the valid side, and
  only that one value is persisted;
- a stored $D$ is only a solver hint under the current curve-formula revision;
  it never narrows the authoritative global bracket, and the resulting
  canonical root is still checked against its adjacent atom locally;
- exact-input output uses the conservative canonical $D$;
- exact-output input rounds upward;
- exact-input output is the maximal raw amount on the valid side of the
  adjacent-atom reserve bracket; output plus one atom is rejected;
- adaptive common-coordinate inversion uses the proved floor/ceiling identity:
  the selected raw atom maps to the certified common endpoint, while its raw
  predecessor maps strictly below it, so execution does not repeat expensive
  endpoint residual solves after conversion;
- unresolved marginal-price or tail proofs reject the operation.

Positive-concentration inner states require at least one whole NAD-normalized
common-value unit ($10^9$ common atoms) on each side. Exact CPMM tails bypass
that floor because their marginal price is the explicit raw reserve ratio.
Initialization, partial withdrawal, and every other transition reject an
unsupported inner state; a final public yLP exit may park the permanently
burned `MIN_LIQUIDITY` dust, and a later supported two-sided deposit rebuilds
the exact curve checkpoint. This is an intentional minimum-useful-depth
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
`trade_end_price_nad`. It excludes retained surcharge because that surcharge is
principal funding rather than external traded flow. The separate
`reserve_end_price_nad` is the final executable-reserve marginal price used by
the next quote, hLP safety checks, and risk observation.

At most one controller target may be admitted per genuine user operation and
at most one center or ramp movement may occur in a slot. A center candidate
moves only toward the pre-trade EMA, by at most the configured adjustment step,
and never crosses it. The controller evaluates one full target. If protected
profit cannot fund that target, the pool moves nothing, freezes the exact
target and required budget, and lets the user operation continue. A later user
operation retries only after retained funding becomes sufficient or current
reserves invalidate the cached calculation. A target that already exceeds the
1% hard impairment cap is marked saturated instead: reserve motion does not
re-run that impossible request on every operation, and only governance changing
the request (or an EMA reversal cancelling a center target) clears it.

Swaps and swap-like leverage operations advance the controller lazily before
freezing their quote. They accrue the relevant debt side, decay the EMA and
volatility state, admit at most one funded controller target, and only then
compute predictive hLP positioning and the trader quote against that applied
curve state. This ordering prevents hLP from positioning against a curve that
the same operation immediately replaces. Residual-exposure safety still spans
the complete controller-plus-trade path: it compares the pre-controller
marginal price with the final reserve endpoint, so controller movement cannot
hide a net worsening of an actionable residual. There is no keeper-only or
auxiliary controller instruction and no external price input. Without user
activity there is no new internal price observation, so no state advances.

With active hLP supply, each genuine operation recomputes correction from
actual inventory and applies the maximum safe amount inline. A tiny,
low-precision, cash-constrained, or insolvent remainder stays explicit for the
next operation; it is never executed later as a stale stored token delta. New
deposits into a target hLP vault are rejected while that vault carries an
actionable remainder, while exits and restoring flow stay live. The opposite
hLP product is not frozen by a one-sided remainder. A due parameter-ramp point
gates both directions so an entrant cannot mint against a pre-ramp NAV basis.
Cash and solvency checks always apply.

Every persisted hLP checkpoint recomputes exposure from actual post-transition
inventory and debt. Residual exposure is recognized as zero only when it is at
most `0.00001` target tokens **and** at most one part per million of current hLP
NAV. Larger residuals—including low-precision token granularity—remain as residual exposure
and are never relabeled as harmless rounding. Normally they pause new hLP entry
until a later genuine operation or an hLP exit makes a smaller correction
executable.

There is one narrow admission exception for a production-controller endpoint:
the uncapped proportional hedge plan must prove that integer raw-token or yLP
rounding cannot express one complete correction. A top-up is then accepted
only if the post-deposit residual is still settled or controller-granularity
limited, keeps its sign unless it reaches zero, does not grow in absolute
value, and does not grow per unit of NAV or hLP supply. The signed residual
remains stored and its settlement reference does not advance. An actionable,
cash-constrained, zero-NAV, or unhedgeable vault cannot use this exception.
Residual exposure is not itself an exit gate and never blocks the lazy curve
controller.

If integer share accounting rounds an active vault's target-side claim to zero,
no finite proportional hedge adjustment exists. Dusk records the signed
opposite exposure as a fail-closed residual signal and performs no no-op reserve
mutation. An underwater vault is likewise recorded with zero NAV and nonzero
residual exposure. Active zero-NAV vaults reject new hLP entry even when their
opposite exposure is exactly neutral. These vault-local states do not make
restoring user operations fail.

Decoupling controller progress from hedge execution is a liveness tradeoff:
unresolved hLP exposure can persist across bounded funded center or parameter
steps. It remains explicit in state; the hLP never receives an implicit
favorable mark merely because the controller advanced. The cached settlement
reference advances only after residual exposure reaches zero, so a partial or
no-op correction—and a granularity-limited top-up—cannot ratchet the allowed
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
finite transition is not free: it is solved and admitted through the same
protected-profit gate. This gives Dusk a protocol-native lazy recentering zone
without pretending that entry back into concentrated liquidity has no cost.

For CPMM, the center is not part of the trading invariant. It remains an
internal divergence-fee anchor and may move without claiming a fictitious
curve impairment.

## Protected Recentring Budget

Dusk Concentrated AMM exposes balanced-equivalent liquidity in the active
common numeraire:

$$
Q^2=
\begin{cases}
\dfrac{D^2\,\mathrm{NAD}}{4c}, & c\ge\mathrm{NAD},\\[6pt]
\dfrac{D^2c}{4\,\mathrm{NAD}}, & c<\mathrm{NAD},
\end{cases}
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

If the full target exceeds the funded or capped budget, Dusk freezes that exact
target and moves nothing. It never labels an underfunded adjustment safe and it
does not search for a smaller favorable candidate.

Retaining surcharge changes executable reserves. The lazy controller therefore
stores an exact target together with its reserve identity and funding
requirement:

1. A genuine operation first retries a previously frozen funded target.
2. If no frozen target is valid, it evaluates one due absolute-time ramp point.
3. If no ramp point is due, it evaluates one center step toward the pre-trade
   EMA.
4. A funded target executes through the exact impairment gate. An underfunded
   target is frozen without moving LP principal. An EMA reversal cancels a
   stale center target; materially changed reserves force fresh evaluation of
   an underfunded target. A cap-saturated request stays dormant until its
   governance request changes, avoiding an expensive impossible solve on each
   operation.

The target controls retention and recenter liveness; it never authorizes
spending. Every actual center or parameter candidate is independently solved,
capped, and checked against current protected profit before mutation.
The 1% `retention_hard_cap_nad` caps the protected-liquidity target and the
amount a controller step may spend; it is not a cap on the trader's
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
- the marginal toll rises monotonically until its configured Huber cap.

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

Before capping, the raw marginal toll grows approximately linearly far from the
center:

$$
F'(u)\sim
\frac{8\kappa}{3\,\mathrm{NAD}\,q_0}\,u
\xrightarrow[u\to\infty]{}\infty.
$$

For divergence share cap $c$ in basis points, Dusk converts the gross-input
share into a toll-per-executable-input marginal cap:

$$
m_{\max}=\left\lfloor
\frac{c\,\mathrm{NAD}}{10{,}000-c}
\right\rfloor.
$$

Let $u_*$ be the first raw coordinate where the uncapped derivative reaches
$m_{\max}$. The production potential is Huberized:

$$
\widehat F(u)=
\begin{cases}
F(u), & u\le u_*,\\
F(u_*)+\left\lfloor (u-u_*)m_{\max}/\mathrm{NAD}\right\rfloor,
& u>u_*.
\end{cases}
$$

Endpoint differencing remains additive and telescoping. The implicit endpoint
solver also embeds the per-swap gross budget
`floor(gross_input * c / 10_000)` rather than clipping after solving.

This targets trending/adverse flow that pushes inventory farther from the
concentrated region.

The configured divergence coefficient is bounded to keep governance and
fixed-point arithmetic inside the audited domain. The coefficient scales the
near-center potential; the separate share cap bounds both its far-tail
marginal rate and its gross-input debit.

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

The configured coefficient and accumulator caps bound $p$. The resulting
volatility debit is then limited by its explicit gross-input share budget.

Base, divergence, and volatility component caps must sum to at most 5,000 bps.
For gross input $g$, total fees are at most $\lfloor g/2\rfloor$, so every
accepted quote sends at least $\lceil g/2\rceil$ into curve execution.

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
claimable lanes. Swap-fee liabilities remain in reserve-vault custody but outside
executable `cash_reserve`; interest liabilities remain in the interest vault.

## Lending, Liquidation, Leverage, And hLP

Risk does not use an external oracle or reconstruct a CPMM when the live market
is concentrated.

- Current price and $Q$ come from the exact applied Dusk Concentrated AMM curve.
- Pessimistic underwriting/liquidation prices come from the existing internal
  EMA books.
- At each pessimistic price and conservative $Q$, Dusk reconstructs the
  corresponding Dusk Concentrated AMM reserve shape, then evaluates impact on that same curve.
- Risk-sensitive operations reconstruct the required pessimistic shapes from
  the latest scalar price/$Q$ observation and the applied center/shape. Those
  large reserve shapes are not persisted in every market; `curve_revision`
  and `risk_revision` make stale scalar risk state explicit.
- Swap preview accrues interest, advances clock observations, runs the hLP
  pre-solver, and simulates the same single eligible ramp or center target on a
  cloned market. Its account is read-only, so preview cannot persist that state.
- Leverage trades use the same quote and dynamic-fee engine as spot, so they
  cannot bypass the surcharge.
- hLP pre-positioning distinguishes trader quote input from physical reserve
  credit, and its exact post-correction is valued on the final applied curve.
- Each debt asset has one shared 24-hour leaky/token bucket for gross new
  principal from fixed lending, isolated leverage, direct hLP funding, and
  automatic hLP funding. Continuous refill carries fractional remainder so
  checkpoint frequency cannot change capacity while the absolute limit is
  fixed; conservative-depth changes may resize that bps-derived limit. This is
  not an exact trailing-window sum, and repayments or exits do not refund
  consumed flow capacity.

This satisfies “lending matches swaps” at the **invariant-mechanics** level,
not at the net voluntary-swap-proceeds level. Lending underwriting, health, and
liquidation references evaluate the fee-free pessimistic curve in
`math/risk.rs` and `market/lending.rs`. The auction bidder transfers debt
tokens directly for collateral, so no AMM fee is mechanically owed.

The selected release policy is the **external-auction model**.
`settle_liquidation_auction_floor` becomes eligible only after the decaying
auction price reaches its stored floor. The liquidator supplies debt tokens
from its own account and receives seized collateral directly; the instruction
does not route collateral through the AMM, waive an AMM fee, or guarantee AMM
exit proceeds. Insurance draws and bounded socialized loss settle accounting
shortfalls but do not manufacture an external bidder.

Liquidation liveness therefore assumes independent collateral demand and an
externally capitalized participant with access to the debt token. If no such
participant accepts the floor, Dusk cannot guarantee that the position will be
liquidated. This is an explicit protocol assumption, not an AMM-backed
liquidation-liveness guarantee and not an external-oracle dependency.

## Configuration And Upgrade Path

The Dusk Concentrated AMM-ready `Market` account embeds:

- current and applied curve parameters;
- center, price EMA, volatility accumulator, and observation slots;
- protected-liquidity floor, budget, and target;
- active timelocked ramp state;
- canonical Dusk Concentrated AMM invariant value, authoritative shape-only
  geometry cache, curve-formula revision, final marginal observation, and
  curve/risk revisions;
- sticky-target state used by the retained-surcharge compute path;
- reserved configuration extension bytes.

Because this layout lands before public deployment, a later contract upgrade
can enable or tune concentration for an existing market without resizing or
migrating that market account. Pre-launch development accounts using an older
layout must be recreated.

The persisted curve-formula revision is checked before an invariant hint or
shape-geometry cache is reused. A later mathematical upgrade makes old
artifacts cold automatically; the next genuine operation recomputes them
under the new formula. That is cache invalidation, not a stateful migration.

The two serialized curve fields are `peak_depth_nad` and
`fade_scale_nad`. This layout is pre-launch and intentionally has no
compatibility promise for accounts created by earlier experimental builds;
those development accounts must be recreated.

Direct-yLP governance changes target parameters only after strict-majority
support and a seven-day timelock. The applied concentration parameters then
move through the funded, one-step-per-slot ramp:

$$
\mathrm{CPMM}\ \longleftrightarrow\ \mathrm{Concentrated}(P,s),
$$

or between two concentrated profiles,

$$
\mathrm{Concentrated}(P_1,s_1)
\ \longleftrightarrow\
\mathrm{Concentrated}(P_2,s_2).
$$

Peak depth and fade scale are one mode switch. CPMM-boundary ramps
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
- The finite transition removes the old one-sided marginal-price gap, but it
  cannot erase the economic catch-up needed to meet an exact CPMM tail. Local
  depth inside part of the transition may be lower than CPMM even while the
  cumulative quote remains better than CPMM. Aggressive `peak_depth` with a
  narrow `fade_scale` compresses that catch-up into sharper curvature, so
  production calibration must bound transition slippage and integer
  sensitivity.
- The exact, fail-closed Dusk Concentrated AMM proofs cost more compute than
  CPMM. Lazy controller work is bounded inside genuine operations; SBF compute
  limits and low-notional precision remain explicit release gates.
- Internal EMA manipulation, wash trading, and same-slot path behavior remain
  adversarial-test surfaces even though the design is oracle-less.

## Release Calibration

Do not select production `peak_depth`, `fade_scale`, fee coefficients,
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
(`peak_depth = fade_scale = 0`) is the mandatory control.
