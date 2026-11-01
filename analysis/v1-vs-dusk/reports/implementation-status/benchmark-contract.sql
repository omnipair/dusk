-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH benchmark_angles(angle, factual_control, dusk_counterfactual, headline_outputs, implementation_status) AS (
  VALUES
    ('Collateral efficiency', 'V1 debt, collateral, capacity, and observed debt-action fractions', 'Native gross health ceiling plus actionable cash/daily/global/reduce-only bounds with exact share rounding', 'Gross/actionable headroom, capacity multiplier, filled/unmet demand, limiting constraints', 'implemented and tested; historical result unavailable'),
    ('Spot capital efficiency', 'Every historical exact-in swap, fee split, min-out, and reserve checkpoint', 'Concentrated curve, same-slot dynamic fees, recentering, hLP settlement, and intent arbitrage', 'Execution cost, slippage, equivalent depth, supported volume, fee return, inventory PnL', 'implemented and tested; historical result unavailable'),
    ('Leverage', 'Bounded V1 borrow-swap-deposit loops from identical historical pre-state', 'Native isolated open/full unwind at 72 cells per eligible lending market', 'Executable leverage, opening/unwind cost, funding APR, thresholds, buffers, realized MAE', 'implemented and tested; historical result unavailable'),
    ('LP and protocol economics', 'V1 native fee, interest, claim, flashloan, cash-covered surcharge, and inventory transitions', 'Exact swap/public-interest/hLP/recenter lanes with user and synthetic origins separated', '15 lane totals, common-capital return, >=30-day APR, cash/inventory memos, shortfall', 'implemented and tested; historical result unavailable')
)
SELECT * FROM benchmark_angles ORDER BY angle;
