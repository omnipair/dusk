# Slot-derived economic time

Dusk measures **economic time** from consensus slot progress. The checked
`slots_to_ms(start, end)` helper converts each scheduled slot to 200 nominal
milliseconds. This is the current Solana slot cadence, established by the
[reduced-slot-time rollout](https://solana.com/upgrades/reduced-slot-times).
It is separate from Alpenglow's approximately 150 ms **finality** target.

The choice is deliberate: under
[Alpenglow](https://solana.com/upgrades/alpenglow), a block leader selects the
block-footer timestamp within consensus bounds. Programs see that value through
`Clock.unix_timestamp` at whole-second resolution. A trader cannot set the
sysvar directly, but a leader can move it within the allowed range; skipped
slots widen that range. Dusk therefore does not use it to advance interest,
moving averages or shared spending budgets. Slot numbers are consensus-ordered,
so the same accepted slot history produces the same economic-time delta without
dependence on a leader's chosen timestamp.

This is **nominal**, not measured wall-clock time. Skipped slots count because
the scheduled slot number advances; a chain halt or a future change in slot
cadence can make slot-derived time differ from elapsed real time. There is no
automatic on-chain wall-time correction. Before deploying on a cluster with a
different slot cadence, update `TARGET_MS_PER_SLOT`, review every literal slot
duration, and rerun the economic calibrations. Do not use the Alpenglow finality
target as the multiplier.

## What uses slots

- Debt-index interest and adaptive interest-rate anchors integrate
  `slot_delta * 200 ms`, retaining the existing one-year per-update cap and
  share/index rounding.
- Price, directional-price, curve-depth, AMM-center, volatility and hLP funding
  averages retain millisecond-denominated half-life parameters, but their
  elapsed input is derived from slots.
- The public borrow bucket refills over a nominal 86,400,000 ms day. The
  insurance draw window opens again after 432,000 slots. Its explicit
  `initialized` flag makes slot zero a valid start.
- Controller cooldowns and protocol auctions use slot-duration parameters.
  Their one-day cap/default is 432,000 slots. Reference freshness, observation
  ordering, token-epoch checks and flash begin/settle binding remain slot/epoch
  checks by design.

Repeated operations in one slot add no economic age. A backwards slot or
overflowing slot-to-millisecond conversion rejects at checked accounting
boundaries. The directional risk bound may still tighten immediately on a worse
observation; that conservative price bound is not time decay. Parameter updates
close the old slot interval under the previous settings before new settings
take effect.

## What still uses the runtime Clock timestamp

Existing absolute Unix-time rules remain intentionally separate: market launch
schedules, governance proposal timelocks and
delegate expiries still read `Clock.unix_timestamp`. Their Alpenglow timestamp
trust model needs a separate policy review if the protocol intends to remove
leader timestamp influence from *all* economic decisions. This timing change
does not silently alter those rules.

## Development interface and replay

The #45 integration branch is undeployed. Its Market layout version remains 6,
now with slot checkpoints and an explicit insurance-window initialization flag
rather than elapsed-timestamp checkpoints. Interest events report from/to
slots. SDK interfaces and native replay inputs follow the slot-driven account
and parameter shapes. Historical joint-shock replay results must be regenerated
after this timing change; they remain a historical auction benchmark, not a
simulation of current emergency/flash liquidation policy.
