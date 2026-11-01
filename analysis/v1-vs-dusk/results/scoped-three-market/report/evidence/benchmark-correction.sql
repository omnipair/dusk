-- Exact report projections generated from benchmark-correction.json inputs.
WITH benchmark_questions(display_order, question, verdict, why) AS (
  VALUES
  (1, 'Collateral efficiency', 'No valid verdict', 'The published values were sums across repeated borrow observations, not debt-to-collateral efficiency ratios; the Dusk candidate was incomplete and unscored.'),
  (2, 'Spot capital efficiency', 'No valid verdict', 'The sampled Dusk candidate produced no comparable execution-cost or slippage observations, so its near-zero revenue is mostly rejected historical flow, not venue economics.'),
  (3, 'Leverage', 'No valid verdict', 'No frozen leverage campaign was executed; projected debt observation-sums are not leverage or maintenance-margin results.'),
  (4, 'LP and protocol economics', 'No valid verdict', 'The Dusk replay deferred 77 external-state operations and was incomplete, so raw revenue totals are not comparable venue revenue or yield.')
)
SELECT question, verdict, why
FROM benchmark_questions
ORDER BY display_order;

WITH publication_blockers(blocker, count) AS (
  VALUES
  ('Score blocking reasons', 7),
  ('Incomplete replay flag', 1),
  ('Null arm candidate score', 1),
  ('Null combined profile score', 1),
  ('Mixed five-/three-market grain', 1)
)
SELECT blocker, count
FROM publication_blockers
ORDER BY count DESC, blocker ASC;
