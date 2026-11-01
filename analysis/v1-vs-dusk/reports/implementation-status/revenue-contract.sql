-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH revenue_lanes(lane, accounting_class, economic_core, interpretation) AS (
  VALUES
    ('lp_swap_earned', 'economic_revenue', 'yes', 'LP swap-fee economics'),
    ('protocol_fee_auction_swap_earned', 'economic_revenue', 'yes', 'Protocol swap fee-auction lane'),
    ('protocol_buyback_swap_earned', 'economic_revenue', 'yes', 'Protocol swap buyback lane'),
    ('retained_recenter_inventory', 'inventory_memo', 'no', 'Protected inventory until economically realized'),
    ('compounded_lp_swap_principal', 'principal_disposition_memo', 'no', 'LP fee amount capitalized into principal'),
    ('lp_public_interest_accrued', 'economic_revenue', 'yes', 'LP public-borrow interest accrual'),
    ('protocol_public_interest_accrued', 'economic_revenue', 'yes', 'Protocol public-borrow interest accrual'),
    ('lp_public_interest_cash_realized', 'cash_realization_memo', 'no', 'Cash realization; V1 remains typed unavailable'),
    ('protocol_fee_auction_public_interest_cash_realized', 'cash_realization_memo', 'no', 'Fee-auction public-interest cash'),
    ('protocol_buyback_public_interest_cash_realized', 'cash_realization_memo', 'no', 'Buyback public-interest cash'),
    ('hlp_funding_interest_earned', 'economic_revenue', 'yes', 'Net hLP funding earned by hLP providers'),
    ('protocol_fee_auction_hlp_funding_interest_earned', 'economic_revenue', 'yes', 'Protocol fee-auction share of hLP funding'),
    ('protocol_buyback_hlp_funding_interest_earned', 'economic_revenue', 'yes', 'Protocol buyback share of hLP funding'),
    ('flashloan_fee_v1_only', 'venue_only_supplement', 'no', 'V1-only supplement outside apples-to-apples core'),
    ('protocol_claim_realized_v1_only', 'cash_realization_memo', 'no', 'V1 claim realization, never new revenue')
)
SELECT * FROM revenue_lanes ORDER BY lane;
