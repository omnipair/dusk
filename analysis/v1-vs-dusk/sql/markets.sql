-- Read-only current market catalog. Historical configuration MUST be rebuilt
-- from finalized initialization/admin instructions, not inferred from this row.
SELECT jsonb_build_object(
    'pair_address', pair_address,
    'visible', visible,
    'token0', token0,
    'token1', token1,
    'lp_mint', lp_mint,
    'rate_model', rate_model,
    'swap_fee_bps', swap_fee_bps::text,
    'half_life', half_life::text,
    'fixed_cf_bps', fixed_cf_bps::text,
    'target_util_start_bps', target_util_start_bps::text,
    'target_util_end_bps', target_util_end_bps::text,
    'rate_half_life_ms', rate_half_life_ms::text,
    'min_rate_bps', min_rate_bps::text,
    'max_rate_bps', max_rate_bps::text,
    'params_hash_hex', encode(params_hash, 'hex'),
    'version', version::text,
    'configuration_history_source', 'rpc_required'
)::text AS row_json
FROM pools
WHERE pair_address IS NOT NULL
ORDER BY pair_address;
