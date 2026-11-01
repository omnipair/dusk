-- Stable keyset page over provider-observed hourly USD price marks.
-- Parameters:
--   $1 inclusive bucket start (Unix seconds)
--   $2 exclusive bucket end (Unix seconds)
--   $3/$4/$5 last bucket Unix seconds/mint/provider cursor
--   $6 page size
--   $7 comma-separated visible-market mint allowlist
--   $8 single configured historical provider
--
-- `bucket` is an external provider observation bucket, not Solana chain time.
-- Only genuinely historical, positive, fixed-point marks are exported. Current,
-- estimated, missing, non-hourly, NaN, infinity, and exponent text are excluded.
WITH historical_marks AS (
    SELECT
        mint::text AS mint,
        EXTRACT(EPOCH FROM bucket)::bigint AS bucket_unix_seconds,
        price_usd::text AS price_usd,
        decimals::integer AS token_decimals,
        provider::text AS provider,
        quality::text AS quality
    FROM token_price_snapshots
    WHERE quality = 'historical'
      AND mint = ANY(string_to_array($7::text, ','))
      AND provider = $8::text
      AND bucket >= to_timestamp($1::bigint)
      AND bucket < to_timestamp($2::bigint)
      AND (EXTRACT(EPOCH FROM bucket)::numeric % 3600) = 0
      AND price_usd > 0
      AND price_usd::text ~ '^(0|[1-9][0-9]*)([.][0-9]+)?$'
), paged AS (
    SELECT *
    FROM historical_marks
    WHERE (bucket_unix_seconds, mint, provider)
        > ($3::bigint, $4::text, $5::text)
    ORDER BY bucket_unix_seconds, mint, provider
    LIMIT $6::bigint
)
SELECT jsonb_build_object(
    'mint', mint,
    'bucket_unix_seconds', bucket_unix_seconds,
    'price_usd', price_usd,
    'token_decimals', token_decimals,
    'provider', provider,
    'quality', quality
)::text AS row_json
FROM paged
ORDER BY bucket_unix_seconds, mint, provider;
