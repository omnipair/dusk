# Elapsed time and slot ordering

Dusk uses Solana `Clock` snapshots at execution boundaries. `MarketClock`
keeps its slot for ordering and converts its signed Unix timestamp from seconds
to checked unsigned milliseconds. Negative timestamps and conversion overflow
reject. No production code estimates elapsed time by multiplying slots by a
fixed duration.

## Alpenglow behavior checked on 2026-10-23

[Solana's Alpenglow documentation](https://solana.com/upgrades/alpenglow) describes
a leader-selected, monotonic timestamp bounded relative to the parent and elapsed
slot duration. Consecutive blocks may share a timestamp. This is a consensus
clock with possible skew, not an exact wall clock or a fixed drift guarantee.
[Agave's runtime implementation](https://docs.rs/solana-runtime/latest/src/solana_runtime/bank.rs.html)
converts the footer's nanoseconds to whole seconds before storing
`Clock.unix_timestamp`; Dusk does not gain subsecond precision by converting it
to milliseconds.

The separate [slot-duration change](https://solana.com/upgrades/reduced-slot-times)
reduced the nominal cadence from 400 ms to 200 ms. Alpenglow's roughly 150 ms
finality target describes finalization latency, not slot duration. Neither
number is an input to Dusk's economic timers.

## Economic state

- Debt indexes and adaptive rate anchors use elapsed timestamp milliseconds.
  Credit, margin and hLP funding retain their existing share/index rounding.
- Price, directional-price, curve-depth, AMM-center, volatility and hLP funding
  rate averages use their configured half-lives against elapsed time.
- The public borrow bucket refills at its daily rate over 86,400,000 ms, carrying
  its fractional refill remainder across checkpoints. Insurance refreshes its
  shared spending budget after the same elapsed duration.
- Center-controller cooldowns use `min_adjustment_interval_ms`; protocol auctions
  use `duration_ms` and a stored start timestamp. Both retain slot ordering.
- Existing Unix-time launch fees, liquidation incentives, governance timelocks
  and delegate expiries continue to use their timestamp schedules.

Advancing slots without advancing the timestamp does not accrue interest or
consume a half-life. A later timestamp integrates the elapsed interval once.
A repeated operation in the same slot cannot advance the economic checkpoint.
Backwards slot/time inputs reject at checked checkpoints. The directional risk
bound can still tighten immediately on a worse observation; that conservative
price bound is not time decay.

Slot-bound flash begin/settle, quote ordering, observation ordering, reference
freshness and token epoch checks remain slot/epoch checks. Their wall-clock
length may change with the network cadence by design.

Parameter updates first close the old interval under the old parameters. A
failed update restores the timestamps together with the economic state. Market
initialization seeds interest and risk timestamps from the current clock.
Insurance uses an explicit initialized flag so timestamp/slot zero is valid.

## Interface and replay

This development change advances the Market layout to v6 and regenerates both
Dusk and delegate interfaces. `BorrowInterestAccrued` retains from/to slots and
adds from/to timestamp milliseconds. Configuration names explicitly expose
milliseconds in Rust, IDL and SDK; no deployed-account migration is assumed.

Native benchmark operations already supply slot plus Unix timestamp and now
use both. Replay callers must supply coherent recorded timestamps. Historical
calibration fixtures may retain a documented synthetic cadence, but that
fixture-only conversion is not available to production code. LiteSVM tests
advance timestamps explicitly for elapsed-time behavior; `warpToSlot` alone
continues to test ordering without inventing elapsed time.
