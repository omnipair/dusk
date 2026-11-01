-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH pipeline_stages(stage_order, stage, purpose, primary_artifacts, acceptance_rule) AS (
  VALUES
    (1, 'extract', 'Export a repeatable-read database snapshot and enrich it with finalized archival RPC evidence.', 'Ordered operations/events, configs, prices, transfer-fee proofs, Parquet', 'Complete certified signature/page/block chains and exact typed decoding'),
    (2, 'audit', 'Reconcile DB and RPC sources and calculate global plus per-market eligibility.', 'data-quality.json and coverage evidence', 'Every non-USD hard gate passes; missing prices only remove USD claims'),
    (3, 'reconcile-v1', 'Replay V1 natively and prove terminal raw-unit equality at the cutoff.', 'V1 replay ledger, terminal proof, universe/common window, 60/20/20 splits', 'Zero raw-unit differences plus exact-cutoff terminal snapshot equality'),
    (4, 'calibrate', 'Fit the training-only interest schedule and search global/per-market Dusk profiles.', 'Interest schedule, calibration evidence, Pareto set, profile freeze', 'Validation gates pass and holdout remains sealed before freeze'),
    (5, 'evaluate', 'Replay the sealed holdout across both behavior arms and both hLP scenarios.', 'Paired metrics, leverage campaigns, sensitivity, metrics manifest', 'Safety/execution gates and conservation identities remain exact'),
    (6, 'verify', 'Recheck every manifest/source hash and execute the bounded notebook.', 'verification.json and benchmark.executed.ipynb', 'Manifest, notebook sandbox, exact values, and source commitments pass'),
    (7, 'render', 'Build technical, governance, and public portable reports from one manifest.', 'artifact.json, provenance.sql, report.html, render receipt', 'Canonical packaging and Chromium desktop/mobile verification pass')
)
SELECT * FROM pipeline_stages ORDER BY stage_order;
