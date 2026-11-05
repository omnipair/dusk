# Junior-first credit loss waterfall

After collateral and borrower equity are recovered, residual funded credit losses
consume junior backing first. Junior can be fully wiped in one settlement; only
the remainder reduces senior backing. This applies to ordinary borrowing,
isolated leverage, and hLP funding. Unpaid interest is canceled separately and
is not converted into principal loss. Existing funded income claims retain their
own accounting.

There is no standalone insurance fund, insurance token vault, coverage percentage,
daily draw budget, funding target/taper, or donation/fortify instruction. Market
creation and liquidation builders do not derive or accept insurance accounts.
The market account uses layout 12; the parameter ABI has seven families and no
insurance update variant.

Junior exit requests and renewals charge a fixed **50 bps** in receipt units.
Adding to an unready request charges only the newly added receipts; renewal
charges the outstanding amount. Exit timing and existing ticket dates remain
protocol-defined. Senior and hLP withdrawals do not gain this fee.

Borrower and leverage liquidations charge a fixed **300 bps** of net debt repaid, capped by recovered
surplus so the charge cannot create additional bad debt. The existing protocol
share is deducted once; all remaining underwriting revenue goes to junior.
Neither fee is a creation setting or a governed/operator parameter. Collateral,
price/health, transfer-fee, cash-backing, and minimum-payment checks still apply.

For example, a 100-unit residual credit loss with 60 units of junior backing wipes
junior and allocates the remaining 40 units to senior. The protocol does not stop
at a fractional junior-coverage limit or postpone junior losses to a later day.
