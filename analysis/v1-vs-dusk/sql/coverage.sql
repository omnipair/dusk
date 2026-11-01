-- Compact raw-table profile for the requested slot window. Timestamp columns
-- are excluded because the indexer populated them with ingestion wall-clock.
WITH profiles AS (
    SELECT 'swaps'::text AS source_table, COUNT(*)::bigint AS row_count,
           COUNT(DISTINCT tx_sig)::bigint AS distinct_transactions,
           MIN(slot::numeric::bigint) AS min_slot, MAX(slot::numeric::bigint) AS max_slot,
           COUNT(*) FILTER (WHERE tx_sig IS NULL OR slot IS NULL)::bigint AS missing_identity,
           COUNT(*) FILTER (WHERE instruction_path IS NULL)::bigint AS missing_order,
           COUNT(*) FILTER (WHERE lp_fee IS NULL OR protocol_fee IS NULL)::bigint AS missing_raw_amounts
    FROM swaps WHERE slot IS NOT NULL AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'adjust_liquidity', COUNT(*), COUNT(DISTINCT tx_sig),
           MIN(slot::numeric::bigint), MAX(slot::numeric::bigint),
           COUNT(*) FILTER (WHERE tx_sig IS NULL OR slot IS NULL),
           COUNT(*) FILTER (WHERE instruction_path IS NULL), 0
    FROM adjust_liquidity WHERE slot IS NOT NULL AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'user_lp_position_updated_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot::numeric::bigint), MAX(slot::numeric::bigint),
           COUNT(*) FILTER (WHERE transaction_signature IS NULL OR slot IS NULL),
           COUNT(*) FILTER (WHERE instruction_path IS NULL), 0
    FROM user_lp_position_updated_events
    WHERE slot IS NOT NULL AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'adjust_collateral_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot), MAX(slot), COUNT(*) FILTER (WHERE transaction_signature IS NULL), COUNT(*), 0
    FROM adjust_collateral_events WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'adjust_debt_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot), MAX(slot), COUNT(*) FILTER (WHERE transaction_signature IS NULL), COUNT(*), 0
    FROM adjust_debt_events WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'user_position_updated_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot), MAX(slot), COUNT(*) FILTER (WHERE transaction_signature IS NULL), COUNT(*), 0
    FROM user_position_updated_events WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'user_position_liquidated_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot), MAX(slot), COUNT(*) FILTER (WHERE transaction_signature IS NULL), COUNT(*), 0
    FROM user_position_liquidated_events WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT 'update_pair_events', COUNT(*), COUNT(DISTINCT transaction_signature),
           MIN(slot), MAX(slot), COUNT(*) FILTER (WHERE transaction_signature IS NULL),
           COUNT(*) FILTER (WHERE instruction_path IS NULL), 0
    FROM update_pair_events WHERE slot BETWEEN $1::bigint AND $2::bigint
)
SELECT jsonb_build_object(
    'source_table', source_table,
    'row_count', row_count,
    'distinct_transactions', distinct_transactions,
    'min_slot', min_slot,
    'max_slot', max_slot,
    'missing_identity', missing_identity,
    'missing_order', missing_order,
    'missing_raw_amounts', missing_raw_amounts
)::text AS row_json
FROM profiles
ORDER BY source_table;
