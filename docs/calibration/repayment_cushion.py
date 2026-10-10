"""Economic predicate for the proposed solvent AMM close, not runtime code.

Output is spendable debt-token sale proceeds, after the modeled execution fees.
The path sweep includes the AMM fee, but not Token-2022 transfer legs. A runtime
implementation must measure all actual credits/debits, use integer rounding, and
roll back the whole transaction when final repayment fails this condition.
"""

import math


def solvent_amm_sale(*, principal, interest, output, equity, maintenance,
                     cushion_limit, reward_max, insurance_rate):
    """Return a fully funded allocation or None; never draw insurance.

    Re-evaluate on actual execution output, not just a pre-execution quote. The
    keeper funds its own transaction cost from its reward; that cost is not a
    caller-supplied debit against the position. Insurance funding is bounded by
    the surplus remaining after principal AND interest are paid.
    """
    values = (principal, interest, output, equity, maintenance, cushion_limit,
              reward_max, insurance_rate)
    if not all(math.isfinite(v) for v in values):
        raise ValueError("Non-finite economic input")
    if (min(principal, interest, output, cushion_limit, insurance_rate) < 0
            or not 0 < maintenance < 1 or not 0 <= reward_max < 1):
        raise ValueError("Invalid economic input")
    debt = principal + interest
    if debt <= 0 or equity > maintenance:
        return None
    severity = min(1, max(0, (maintenance - equity) / maintenance))
    reward = output * reward_max * severity
    repayment = output - reward
    # No tolerance can authorize a loss. This is floating-point analysis, not a
    # token-atom rounding specification for the eventual on-chain instruction.
    if repayment < debt:
        return None
    contribution = min(repayment - debt, debt * insurance_rate)
    owner = repayment - debt - contribution
    cushion = owner / debt
    if cushion > cushion_limit:
        return None
    return dict(principal_paid=principal, interest_paid=interest, reward=reward,
                contribution=contribution, owner=owner, cushion=cushion,
                repayment=repayment, insurance=0.0, loss=0.0,
                canceled_interest=0.0)
