-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH blockers(blocker, current_state, why_it_blocks, required_evidence) AS (
  VALUES
    ('Historical decoder dispatch is provisional', 'One all-slot provisional v0.10.5 decoder; headline_verified=false', 'Historical upgrades cannot be attributed to exact deployed binary/IDL revisions from labels alone.', 'Sealed Program/ProgramData/buffer histories, reconstructed deployed ELF hashes, and order-aware revision decoders'),
    ('Exact terminal snapshot is not supplied', 'Runner validates but deliberately does not fabricate or fetch current-state substitutes', 'Transition replay cannot be accepted until every terminal account equals the exact cutoff state.', 'Independent binding plus full requested account set with response_context_slot exactly equal to cutoff')
)
SELECT * FROM blockers ORDER BY blocker;
