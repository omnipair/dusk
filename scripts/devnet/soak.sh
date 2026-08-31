#!/usr/bin/env bash
#
# Watch the deployment run unattended.
#
# Phase 8 asks for sustained operation, which is not something a test asserts —
# it is something you observe for long enough that a slow leak has room to show
# itself. This samples the public surface on an interval and records one line
# per sample, so the record is a time series rather than an impression.
#
# It changes nothing. Anything that needs to write belongs in a test, not in a
# soak, because a soak that mutates cannot tell you whether the thing it is
# watching would have been stable on its own.
#
#   scripts/devnet/soak.sh [interval_seconds] [samples]
set -euo pipefail

API="${DUSK_API_URL:-https://dusk-api-production-291f.up.railway.app}"
INTERVAL="${1:-300}"
SAMPLES="${2:-288}"
LOG="${SOAK_LOG:-./soak-$(date -u +%Y%m%dT%H%M%SZ).jsonl}"

echo "sampling ${API} every ${INTERVAL}s, ${SAMPLES} times -> ${LOG}"

for ((i = 1; i <= SAMPLES; i++)); do
  STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  # A failed curl must produce a row rather than a gap: an absent sample is
  # indistinguishable from a sample nobody took, and the outage is the thing
  # worth recording.
  BODY="$(curl -s -m 20 "${API}/api/dusk/v1/status" || echo '{}')"
  # The stamp goes through the environment rather than being interpolated into
  # the script. ${VAR@Q} needs bash 4.4 and macOS ships 3.2, so a soak started
  # by hand on a laptop — the likeliest way it gets started — would fail on its
  # first sample.
  ROW="$(printf '%s' "${BODY}" | SOAK_STAMP="${STAMP}" python3 -c "
import json, os, sys

stamp = os.environ['SOAK_STAMP']
try:
    data = json.load(sys.stdin)['data']
    print(json.dumps({
        'at': stamp,
        'reachable': True,
        'slotLag': data.get('slotLag'),
        'events': data.get('indexedEvents'),
        'degraded': data.get('degraded', []),
    }))
except Exception as error:
    print(json.dumps({'at': stamp, 'reachable': False, 'error': str(error)[:80]}))
")"
  printf '%s\n' "${ROW}" | tee -a "${LOG}"
  [ "${i}" -lt "${SAMPLES}" ] && sleep "${INTERVAL}"
done

echo "--- summary"
python3 - "${LOG}" <<'PY'
import json, sys

rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
reachable = [row for row in rows if row.get('reachable')]
lags = [row['slotLag'] for row in reachable if isinstance(row.get('slotLag'), int)]
degraded = [row for row in reachable if row.get('degraded')]

print(f"samples          {len(rows)}")
print(f"unreachable      {len(rows) - len(reachable)}")
print(f"degraded         {len(degraded)}")
if lags:
    ordered = sorted(lags)
    print(f"slot lag         min {ordered[0]}  median {ordered[len(ordered)//2]}  max {ordered[-1]}")
if reachable:
    first, last = reachable[0].get('events'), reachable[-1].get('events')
    print(f"events ingested  {first} -> {last}")
# Stable is not the same as healthy: an indexer that never advances is
# perfectly stable and completely broken.
if lags and lags[-1] > 15_000:
    print("ENDED DEGRADED: slot lag is past the threshold")
    sys.exit(1)
PY
