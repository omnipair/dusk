# LP interest accounting

Implementation target: a separate PR into `fix/benchmark-replay-owner` (PR #45),
starting at `5c0aca7fcb582418f1c5b99061d4ff7605b6f185`.

## Selected behavior (2026-10-18)

- Keep V1-style ownership: accrued public borrower interest is included in LP
  share value. Entrants pay for their share of the existing receivable.
- Apply the same economic backing to yLP mint/burn and hLP share valuation.
- An exiting holder receives the permitted cash withdrawal and retains no
  separate unpaid-interest entitlement. Existing funded yield is distinct.
- Repayment converts a receivable into cash; it must not count the same income
  twice. Cancellation removes the receivable from current share value.
- Cash availability and backing checks remain binding. Notional reserves do
  not authorize spending tokens the reserve vault does not hold.
- Preserve the existing swap-fee split: compounded fees enter share principal;
  separate swap fees retain their funded per-holder index.
- Keep the current AMM pricing model: public unpaid interest remains excluded
  from `curve_reserve`. The later V1-style swap-pricing proposal was declined.

The previously proposed historical claim ledger and proportional funding
policy are superseded. They are not part of this implementation.

## Accounting boundary under investigation

V1 accrues interest before pricing new LP shares, so buying old receivables does
not give a newcomer free earnings. Dusk yLP already follows this rule. Dusk hLP
also marks its entry NAV using live reserves, but chooses its deposit legs and
checks its hedge against curve reserves that exclude public unpaid interest.
The resulting inconsistent deposit basis must be resolved without granting
newcomers discounted shares or silently relaxing hedge protection.

V1 also includes interest in AMM pricing reserves. That pricing behavior is not
part of the selected Dusk model.

## Local implementation and evidence

A local experiment chose the opposite funding leg from live reserves to match
the underlying yLP mint. It was incomplete and has been removed from candidate
runtime; its patch is retained in the local audit evidence.

The native reproduction used six-decimal assets, 1,000,000 tokens on each side,
20,000 existing hLP tokens, 400,000 public principal, 25,000 unpaid interest and
a 5,000-token hLP deposit. It covered both interest assets and both vaults at
1x and 4x concentration. Initially all eight entries hit the post-entry hedge
guard. With the corrected ratio, the four target-asset-interest cases execute;
the four opposite-asset-interest cases still hit that guard. Fixture EMA prices
are initialized to 1, so this is not an uninitialized-oracle rejection.

The retained regression checks yLP minting and a cash-funded round-trip
withdrawal: old holders keep their full live-reserve claims, the newcomer buys
at the accrued value, and exiting does not create an unpaid-interest claim.

This is not a complete fix. Keeping interest excluded from swap pricing requires
an economic entry/rebalancing design that prices
the acquired receivable without shifting its cost to existing holders.

hLP funding interest has a separate ordinary-yLP eligibility denominator. The
implementation must preserve that exclusion as well as public interest passing
through vault-owned yLP; neither source may accrue its revenue twice.

## Required integration coverage

Interest accrual and repayment, partial/full liquidation and writeoff, yLP and
hLP deposits/withdrawals, token-transfer hooks, governance custody and delegation,
harvest, SDK previews and generated IDLs. Check amount rounding, transfer fees,
funded-custody conservation, future protocol-fee changes, concentrated curves,
and swaps which settle hLP funding interest.

The remaining pricing/hedge boundary must be resolved before #45 can merge or
deploy. Narrow cleanup/SDK fixes can be reviewed independently, but do not close
this accounting work. See [the remediation ledger](audit/MODEL_REVIEW_CLOSEOUT.md).
