# Health-only atomic liquidation — 2026-10-30

This decision supersedes historical time-incentive and committed-distress designs.
Borrowing and leverage liquidations require no registration transaction or stored
liquidation episode. `preview_liquidation_health` is a read-only simulated view;
it does not authorize a later transaction. Begin and emergency execution refresh
interest/risk and independently check current health. Begin fixes the payment;
settlement cannot change it based on time or later health.

For EMA equity fraction e and applicable maintenance m, let
p = clamp(1 - e/m, 0, 1). Whole-basis-point arithmetic follows the program:

- Ordinary maximum discount: 50 + floor(450*p) bps (0.5–5%).
- Emergency caller reward: floor(100*p) bps, capped at 1% of spendable proceeds.
- Emergency access remains e <= 0.70*m, so its boundary reward is 0.30%.
- Ordinary partial quotes may reduce their discount to preserve improvement,
  down to 50 bps. Every fill must improve surviving health after actual costs.
- Recovery remains surviving MM +2 points; no partial may seize beyond an earlier
  permitted recovery crossing, including a later rounding dip below that target.

Atomic pair binding, measured transfer credits, partial debt-share rounding,
scheduled token-fee protections, full-close proofs and the loss waterfall remain.
Unchanged health gives unchanged incentives even as Unix timestamps advance.
Interest, EMA changes and scheduled fees may still change health and execution.
Partial fills reduce price impact; permitted fills must still cover execution costs.

The old `observe_liquidation` instruction, `LiquidationObserved` event,
`LiquidationDistress` type and position fields, SDK `observeInstruction`,
`liquidationDistressAge`, and incentive age argument are removed. Position layouts
and the borrower preview change; consumers must adopt regenerated interfaces
and the companion keeper candidate together. This change performs no deployment.
