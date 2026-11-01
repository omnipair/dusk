-- Actual independent metric calculation from dynamic_fees.py curve/fee outputs.
-- $1m capital and +1% external jump for LP shortfalls; gross $10k buy for execution.
-- Source curve, fee ordering, optimizer and parameter/state assumptions remain
-- in dynamic_fees.py and dynamic_results.json; inputs below are synthetic.
WITH modeled(ordering, profile, curve_shortfall, base_fee, distance_fee,
             volatility_fee, ten_k_buy_output, matched_execution_capital) AS (
 VALUES
 (0, 'v1 base only', 11.303873771323879, 5.234123330376046, 0, 0.0, 9775.084808910382, 999999.9999996051),
 (1, '5x base only', 56.519322155938426, 26.170569951083674, 0, 0.0, 9930.397574473289, 261446.45083762181),
 (2, '5x dynamic, quiet', 37.21895248898363, 13.738331405265173, 5.030077537989562, 0.0, 9878.985929334944, 624195.2703272153),
 (3, '5x dynamic, warm', 16.70789645530067, 5.449901882870926, 0.31104682351776014, 9.01086596554281, 9830.532851481403, 737990.6255974406),
 (4, '5x dynamic, hot', 0.0, 0.0, 0, 0.0, 9782.550307696802, 949255.6985351259)
)
SELECT ordering, profile,
       curve_shortfall - (0.8*base_fee + distance_fee + volatility_fee) AS lp_loss_distributed_usd,
       curve_shortfall - 0.8*base_fee AS lp_loss_retained_usd,
       distance_fee + volatility_fee AS protected_bucket_if_retained_usd,
       10000*(10000/ten_k_buy_output-1) AS average_cost_bps,
       matched_execution_capital,
       1-matched_execution_capital/1000000 AS capital_saved_same_execution
FROM modeled ORDER BY ordering;
