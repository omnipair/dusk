# Market operator

Dusk has one operator per market, initially the deployer, and an optional manager. LP receipts provide
ownership and income claims, with no core voting power. Supplying liquidity
cannot replace the operator. A wallet or an external program's signer PDA can
hold authority. Token voting, futarchy, token issuance and revenue distribution
may be implemented by that peripheral program.

## Announcements and execution

One pending atomic bundle may contain at most one update per parameter family,
plus operator, manager, revenue recipient and operator interest-share changes.
Only the operator can change these authorities or revenue terms; its manager
may announce updates to the seven parameter families under the same policy. Every
bundle receives the full `OPERATOR_NOTICE_SLOTS`: maximum junior notice plus
one nominal day (4,752,000 slots at 200ms/slot). Execution is permissionless
after maturity and before its seven-day execution window expires. Wall-clock
elapsed time depends on actual slot cadence.

The operator may replace or cancel any announcement. A manager may replace
or cancel only its own bundle, never an operator-authored pending change. Replacement invalidates
the old pending pointer and starts a fresh full notice. There is no monthly
movement budget or announcement cooldown. Pending changes do not escrow LP
tokens or prevent exits. Obsolete, executed and expired proposal accounts may
be closed permissionlessly to their original rent payer.

The immutable digest commits to program, market, operator epoch, nonce,
policy version, exact targets, complete parameter revisions, metadata and
execution dates. Nonces continue across transfers and closed accounts. An
execution checks these commitments and current protocol validity again. A
bundle either executes completely or leaves the market unchanged.

A nonzero successor must sign to accept. Every ownership transfer clears the
manager, with no retain or replacement option in that bundle. The successor
may appoint a manager through a new full-notice announcement.

The operator can immediately remove a manager. This invalidates that manager's
pending bundle; the original payer can recover its rent through permissionless
cleanup. An operator-authored pending bundle is preserved. Removal does not
reset nonces or undo already executed settings. Appointing or replacing a
manager still receives full notice. A separate funded payer can prepare
accounts for a PDA operator. Setting authority to zero permanently renounces
core control and disables any manager; there is no LP election or program-owner recovery route. Revenue
recipient is separate from authority. A nonzero authority transfer defaults
the future recipient to the successor unless the bundle explicitly chooses
another recipient. Renunciation preserves the existing recipient unless an
explicit replacement is supplied.

## Fixed policy across Dusk

| Family | Operator authority | Protection |
| --- | --- | --- |
| Fees | Base/dynamic fee settings, fee asset and compounding | Existing protocol validation; launch schedule fields freeze once launch starts |
| Concentration | Amplification, core/fade widths, including CPMM transitions | Full notice, ramp of at least one nominal day, positive targets within 0.1×–10× starting values; zero-width transitions also require guarded ramps |
| Interest model | Risk-signal and utilization target bands, adaptation half-life | Signal lower < upper; utilization 0–99.99% with lower < upper; half-life 12 hours–30 days; live base APR preserved on edit |
| Price/depth signals | Four EMA half-lives | Existing positive protocol ranges; accrued history is retained |
| Automatic centering | Minimum adaptive step | Always active for concentration; EMA-directed candidate and funded-profit check; adaptive step is max(minimum step, distance/5) |
| Public borrowing | Daily principal outflow limit | Protocol maximum 30%; previously consumed budget remains counted |
| Tranches | Junior share of LP income | 10–90%; old income and existing exit-ticket dates retained |
| Operator economics | Recipient and 0–50% of base interest | Full notice; increasing the fee requires market health and exit-utilization checks |

Oracle provider/feed and oracle policy are creation-only. Initial base APR is
also creation-only. The controller has a fixed 1% base APR floor and no
configurable economic ceiling. Its integer representation still has a finite
numeric limit. No operator update changes the fixed 50bps junior request/renewal
fee, the 300bps liquidation fee, or junior exit timing. There is no standalone
insurance vault, donation instruction, insurance cap or draw budget: funded
losses pass through junior, then senior.

## Numerical accounting boundaries

Virtual reserves use u128 so cash plus indexed receivables do not overflow a
single SPL token amount. Signed debt and collateral event deltas use i128;
settled token amounts, individual debt buckets and cash-backed fee balances
retain their u64 atom domains.

Accrual chooses one representable shared index before changing borrower debt,
LP receivables, or operator/protocol fees. If an accounting domain is exhausted,
additional unrepresentable interest is permanently forgone, the checkpoint
advances, and no excess fractional growth is carried forward. Previously owed
debt and earned cash remain intact. Future accrual resumes only when the coupled
ledgers have capacity; an exhausted LP income denomination can be archived and
rolled after settlement through the existing permissionless income-epoch flow.
The rate controller continues evolving while interest growth is saturated.
A debt-free side resets its index and fractional carry while retaining its APR.

Numerical admission also preserves hLP raw-atom redemption headroom. Saturation
does not turn unpaid income into cash or guarantee an exit from unavailable
liquidity. Repayment, liquidation, and otherwise cash-backed LP exits remain
available. This is a fixed representation policy, not an operator-selected APR cap.

## Geometry and exit protection

Scheduling a concentration ramp does not immediately change geometry. Each
attempted step must preserve funded reserve value, market health and spot
price within fixed-point rounding, compared both with the preceding shape and
the announced starting shape at current reserves and center. Rejected steps
leave the current curve usable. A scheduled candidate is attempted at most once per slot, including rejected attempts; a later slot may retry against current inventory. Withdrawals and repayments do not depend on
successfully advancing the ramp. Zero↔nonzero widths can be announced; CPMM is
a valid target.

The timelock gives notice, not guaranteed immediate cash. Senior and hLP exits
still need available liquidity; junior exits also require their normal notice
and exercise window. Configuration execution checks live utilization and
health. Announcing a change does not reserve liquidity for departing LPs or
rewrite a ticket date. A stalled ramp may be replaced only through a fresh
announced bundle.

## Revenue and global protocol settings

The operator receives 0–5000bps of base interest and no swap revenue. The
FutarchyAuthority signer controls the separate global interest markup. This
signer can be the team or program governance; it is independent of each pool
operator. Markets cache that global value internally, with no pool-specific
markup setting. At the next checkpoint each market first accrues elapsed
interest at its previous value, then uses the latest global value prospectively.

For $100 base interest, 30% operator share and 20% protocol markup:

| Borrower pays | LP income | Operator revenue | Protocol revenue |
| ---: | ---: | ---: | ---: |
| $120 | $70 | $30 | $20 |

The amount allocated to fees is fixed when interest accrues. Recipient
entitlement is fixed when borrower cash is collected. Alice's already collected
cash stays claimable after Bob takes over; unpaid fees collected afterward go
to Bob. Sub-atom rounding carry follows the current recipient. A recipient
change seals the outgoing collected cash in an epoch-addressed account, so
claiming never scans operator history. Public and hLP funding, base and quote,
remain separate accounting sources.

All claims use measured net custody credit, and fees on canceled unpaid
interest do not create cash. Reserve-funded public-interest advances pay LP
income only. Referral commissions come from the protocol allocation, never
from operator income. Fees remain in the existing interest vault with explicit
backed liabilities. Permissionless claims can pay only the recorded recipient's
canonical token account. Swaps and revenue claims do not invoke peripheral
governance.
