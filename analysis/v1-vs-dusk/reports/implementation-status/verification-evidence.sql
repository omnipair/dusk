-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH verification_counts(suite, verified_items, evidence_kind, result) AS (
  VALUES
    ('Benchmark library tests', 164, 'Rust unit tests', 'passed'),
    ('Dusk native benchmark tests', 22, 'Native parity and rollback tests', 'passed'),
    ('Schema contracts', 18, '16 artifact snapshots plus 2 supporting schemas', 'passed'),
    ('Data-quality integration tests', 14, 'Extraction and audit fixtures', 'passed'),
    ('Portable report audiences', 3, 'Chromium desktop and narrow-layout verification', 'passed')
)
SELECT * FROM verification_counts ORDER BY verified_items DESC;
