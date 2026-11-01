-- Stable keyset page over raw V1 event tables.
-- Parameters:
--   $1 inclusive start slot, $2 inclusive end slot
--   $3/$4/$5 last slot/source table/source id cursor
--   $6 page size
-- No database timestamp is exported as chain time.
WITH event_rows AS (
    SELECT
        'swaps'::text AS source_table,
        id::bigint AS source_id,
        'swap'::text AS event_kind,
        pair::text AS pair,
        user_address::text AS actor,
        tx_sig::text AS signature,
        slot::numeric::bigint AS slot,
        instruction_index::integer AS instruction_index,
        instruction_path::text AS instruction_path,
        jsonb_build_object(
            'token0_in', is_token0_in,
            'amount_in', amount_in::text,
            'amount_out', amount_out::text,
            'reserve0_after', reserve0::text,
            'reserve1_after', reserve1::text,
            'lp_fee', lp_fee::text,
            'protocol_fee', protocol_fee::text
        ) AS payload
    FROM swaps
    WHERE slot IS NOT NULL
      AND pair IS NOT NULL
      AND tx_sig IS NOT NULL
      AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'adjust_liquidity', id::bigint, 'liquidity', pair::text,
        user_address::text, tx_sig::text, slot::numeric::bigint,
        instruction_index::integer, instruction_path::text,
        jsonb_build_object(
            'action', event_type::text,
            'amount0', amount0::text,
            'amount1', amount1::text,
            'liquidity', liquidity::text
        )
    FROM adjust_liquidity
    WHERE slot IS NOT NULL
      AND pair IS NOT NULL
      AND tx_sig IS NOT NULL
      AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'user_lp_position_updated_events', id::bigint, 'lp_position',
        pair_address::text, signer::text, transaction_signature::text,
        slot::numeric::bigint, instruction_index::integer, instruction_path::text,
        jsonb_build_object(
            'lp_amount', lp_amount::text,
            'amount0', amount0::text,
            'amount1', amount1::text
        )
    FROM user_lp_position_updated_events
    WHERE slot IS NOT NULL
      AND pair_address IS NOT NULL
      AND transaction_signature IS NOT NULL
      AND slot::numeric::bigint BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'adjust_collateral_events', id::bigint, 'collateral_delta', pair::text,
        signer::text, transaction_signature::text, slot::bigint,
        NULL::integer, NULL::text,
        jsonb_build_object('amount0', amount0::text, 'amount1', amount1::text)
    FROM adjust_collateral_events
    WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'adjust_debt_events', id::bigint, 'debt_delta', pair::text,
        signer::text, transaction_signature::text, slot::bigint,
        NULL::integer, NULL::text,
        jsonb_build_object('amount0', amount0::text, 'amount1', amount1::text)
    FROM adjust_debt_events
    WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'user_position_updated_events', id::bigint, 'position', pair::text,
        signer::text, transaction_signature::text, slot::bigint,
        NULL::integer, NULL::text,
        jsonb_build_object(
            'position', position::text,
            'collateral0', collateral0::text,
            'collateral1', collateral1::text,
            'debt0_shares', debt0_shares::text,
            'debt1_shares', debt1_shares::text,
            'collateral0_max_cf_bps', collateral0_max_cf_bps,
            'collateral1_max_cf_bps', collateral1_max_cf_bps,
            'collateral0_liquidation_cf_bps', collateral0_liquidation_cf_bps,
            'collateral1_liquidation_cf_bps', collateral1_liquidation_cf_bps
        )
    FROM user_position_updated_events
    WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'user_position_liquidated_events', id::bigint, 'liquidation', pair::text,
        signer::text, transaction_signature::text, slot::bigint,
        NULL::integer, NULL::text,
        jsonb_build_object(
            'position', position::text,
            'liquidator', liquidator::text,
            'collateral0_liquidated', collateral0_liquidated::text,
            'collateral1_liquidated', collateral1_liquidated::text,
            'debt0_liquidated', debt0_liquidated::text,
            'debt1_liquidated', debt1_liquidated::text,
            'collateral_price_nad', collateral_price::text,
            'shortfall', shortfall::text,
            'liquidation_bonus', liquidation_bonus_applied::text,
            'k_before', k0::text,
            'k_after', k1::text
        )
    FROM user_position_liquidated_events
    WHERE slot BETWEEN $1::bigint AND $2::bigint

    UNION ALL
    SELECT
        'update_pair_events', id::bigint, 'pair_update', pair::text,
        signer::text, transaction_signature::text, slot::bigint,
        instruction_index::integer, instruction_path::text,
        jsonb_build_object(
            'price0_ema_nad', price0_ema::text,
            'price1_ema_nad', price1_ema::text,
            'rate0_nad', rate0::text,
            'rate1_nad', rate1::text,
            'accrued_interest0', accrued_interest0::text,
            'accrued_interest1', accrued_interest1::text,
            'lp_interest0', lp_interest0::text,
            'lp_interest1', lp_interest1::text,
            'protocol_interest0', protocol_interest0::text,
            'protocol_interest1', protocol_interest1::text,
            'cash_reserve0', cash_reserve0::text,
            'cash_reserve1', cash_reserve1::text,
            'reserve0_after_interest', reserve0_after_interest::text,
            'reserve1_after_interest', reserve1_after_interest::text
        )
    FROM update_pair_events
    WHERE slot BETWEEN $1::bigint AND $2::bigint
), paged AS (
    SELECT *
    FROM event_rows
    WHERE (slot, source_table, source_id)
        > ($3::bigint, $4::text, $5::bigint)
    ORDER BY slot, source_table, source_id
    LIMIT $6::bigint
)
SELECT jsonb_build_object(
    'source_table', source_table,
    'source_id', source_id,
    'event_kind', event_kind,
    'pair', pair,
    'actor', actor,
    'signature', signature,
    'slot', slot,
    'instruction_index', instruction_index,
    'instruction_path', instruction_path,
    'payload', payload
)::text AS row_json
FROM paged
ORDER BY slot, source_table, source_id;
