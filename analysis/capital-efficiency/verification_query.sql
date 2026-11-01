-- Inputs: inspected Dusk curve modeled in model.py, $1m initial capital,
-- $10k gross buy, 30-bps fixed input fee, 100% annual volatility.
-- Tail/shoulder ratios and base_out are floating-point curve outputs.
WITH curve_outputs(amplification, capital_usd, gross_quote, base_out,
                   fee_bps, tail_depth_vs_v1, shoulder_depth_vs_v1) AS (
  VALUES
    (1, 1000000, 10000, 9775.084808910266, 30, 1.0, 1),
    (2, 1000000, 10000, 9863.362567108183, 30, 0.9852543572112978, 1.4926271786056489),
    (5, 1000000, 10000, 9930.397574473289, 30, 0.9410174288451914, 2.970508714422596),
    (10, 1000000, 10000, 9950.159382192127, 30, 0.8672892149016801, 5.43364460745084)
)
SELECT *,
  CASE WHEN amplification=1 THEN 'v1 / 1×' ELSE 'Dusk ' || amplification || '×' END AS configuration,
  10000.0 * (gross_quote / base_out - 1) AS average_cost_bps,
  amplification * capital_usd * 1.0 / (8.0 * 365) AS gross_lvr_usd_day,
  1.0 - 1.0/amplification AS same_peak_depth_capital_saved
FROM curve_outputs
ORDER BY amplification;
