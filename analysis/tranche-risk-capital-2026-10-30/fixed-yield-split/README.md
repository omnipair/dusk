# Fixed 50/50 interest allocation

The subsequent [design and implementation plan](../PLAN.md) adopts this exact
interest split and uses committed junior/debt for coverage pricing, following
the clarification that senior cash exits should not directly change that signal.
The discussion below records the arithmetic and choices at this earlier step.

The user proposes a fixed equal split between senior and junior. This note
evaluates equal shares of distributable interest dollars, not equal APYs.
It does not finalize the still-ambiguous numerator of "junior-to-utilization".

With net collected borrow interest I and funded tranche NAVs J and S:

```
junior interest = I / 2
senior interest = I / 2
junior interest yield = I / (2J)
senior interest yield = I / (2S)
junior yield / senior yield = S / J
```

At J=20, S=80, debt=80, and borrow APR=8%, annual interest is 6.4 before
protocol fees, assuming it is fully paid. Each tranche gets 3.2. Junior's gross
interest APR is 16%; senior's is 4%. These are not loss-adjusted returns or APYs
with compounding. Swap fees and ordinary inventory P&L are excluded.

| Junior | Senior | Junior gross interest APR | Senior gross interest APR |
|---:|---:|---:|---:|
| 10 | 90 | 32.00% | 3.56% |
| 20 | 80 | 16.00% | 4.00% |
| 30 | 70 | 10.67% | 4.57% |
| 50 | 50 | 6.40% | 6.40% |
| 60 | 40 | 5.33% | 8.00% |

Each row reallocates the same 100 capital between tranches; debt and cash stay
fixed. The borrow rate is frozen to isolate allocation. A risk controller could
subsequently raise it as junior leaves and lower it as junior enters, reinforcing
the change in gross junior yield. Investor risk tolerance and actual payment
behavior are not estimated by this arithmetic.

A fixed split is transparent and naturally dilutes junior rewards per dollar
when junior supply grows. It does not guarantee junior always earns the higher
APR: yields equalize at J=S, and above that junior earns less despite first-loss
subordination. That can encourage switching to senior; no cap on junior supply
or automatic amendment of the split is proposed here. When credit exposure is
small, the economic value of subordination can also be small.

## Clean candidate contract

Apply 50/50 to net realized, LP-distributable borrow interest after explicit
protocol charges. Allocate ordinary swap fees by LP economic ownership as a
separate stream. The user could choose to split all net yield equally instead;
that would subsidize underwriting from trading revenue even when borrowing is
small. It is a distinct product decision, not an arithmetic necessity.

The interest split above is exactly 50/50 of the whole eligible interest stream.
It differs from the recent hybrid probes, which allocated half to junior as
underwriting revenue PLUS a pro-rata share of the remaining half. At 20% junior
ownership that older rule gave junior 60% of total interest. Those prior results
must not be described as tests of this exact 50/50 rule.

Reward entitlement must follow time spent bearing the relevant exposure, while
cash payment remains limited to actual collections. A fresh deposit or tranche
switch should not capture an earlier period's interest merely by arriving before
repayment/harvest. Pending junior conversions remain loss liable and earn their
contractual rewards through notice; claims convert at settled NAV. Unpaid
interest is not counted as funded insurance. These are economic requirements;
epoch/index implementation is not supplied or validated here.

When one tranche has zero capital, its nominal yield is undefined and no reward
can be attributed to nonexistent shares. An empty-tranche allocation rule and
entry-time quote must be specified before implementation; do not expose an
infinite-APR quote or let a new depositor acquire past reward entitlements.
No runtime policy has been changed.

## The ratio clarification

Dusk currently defines utilization as D/(D+C), where C is idle executable cash.
For a one-asset funded book T=D+C=S+J, junior share q=J/T. Therefore:

```
q / U = (J/T) / (D/T) = J/D
```

This identity does not claim U stays constant. At J=20, D=80, C=20, q=20%
and U=80%, giving 25%. Senior withdrawal of 10 reduces T to 90: q=22.2222%
and U=88.8889%, again giving 25%. Both ratios change by the same factor.
In contrast, junior AMOUNT/U changes from 25 to 22.5, but its units are capital
and its target depends on pool size. The previously proposed `(J/D)/U` is yet
another signal; it must not be silently substituted for q/U or J/U.
This distinction remains open independently of adopting an equal revenue split.

Reproduce with `python3 calculate.py`. Exact rational checks verify the revenue
allocation and ratio identities. [calculations.json](calculations.json) retains
the values. No native Dusk changes, dynamic equilibrium simulation, risk
calibration or assertion that 50/50 is optimal accompanies this calculation.
