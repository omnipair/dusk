"""Exact one-numeraire examples; no AMM, hLP, fees, rounding or unpaid interest.

Checks economic counterexamples, not full Dusk safety or on-chain feasibility.
"""
from dataclasses import dataclass
from fractions import Fraction as F
import json
from pathlib import Path


@dataclass
class Ledger:
    junior: F = F(20_000)
    senior: F = F(80_000)
    jr_supply: F = F(20_000)
    sr_supply: F = F(80_000)
    cash: F = F(20_000)

    def loss(self, amount):
        first = min(amount, self.junior)
        self.junior -= first
        self.senior -= amount - first
        assert self.senior >= 0

    def convert(self, shares):
        assert 0 <= shares <= self.jr_supply
        assert self.senior > 0 and self.sr_supply > 0
        before = self.junior + self.senior
        cash_before = self.cash
        value = shares * self.junior / self.jr_supply
        issued = value * self.sr_supply / self.senior
        self.junior -= value
        self.senior += value
        self.jr_supply -= shares
        self.sr_supply += issued
        assert self.junior + self.senior == before and self.cash == cash_before
        return issued


def run():
    out = {}
    a, b = Ledger(), Ledger()
    a.loss(F(4_000))
    after_loss = a.convert(F(10_000))
    before_loss = b.convert(F(10_000))
    b.loss(F(4_000))
    assert (after_loss, a.junior, a.senior) == (8_000, 8_000, 88_000)
    assert (before_loss, b.junior, b.senior) == (10_000, 6_000, 90_000)
    out["loss_order"] = {"exit_value_after_loss": float(after_loss),
                         "exit_value_before_loss": float(before_loss),
                         "extra_loss_for_remaining_junior_if_conversion_first": 2_000}

    outcomes = []
    for early in (True, False):
        ledger = Ledger()
        escrow = ledger.convert(F(10_000))
        wallet = F(0)
        if early:
            wallet, escrow = escrow, F(0)
        ledger.loss(F(19_000))
        income = (wallet + escrow) / ledger.sr_supply * 900
        wallet += escrow
        escrow = F(0)
        outcomes.append((wallet * ledger.senior / ledger.sr_supply, income))
    assert outcomes == [(F(9_000), F(100)), (F(9_000), F(100))]
    out["claim_timing"] = {"principal_either_claim_time": 9_000,
                           "senior_income_either_claim_time": 100,
                           "assumption": "Escrowed sr shares participate in senior rewards and losses."}

    alice, bob = after_loss * F(3, 10), after_loss * F(7, 10)
    assert alice + bob == after_loss
    out["batch_distribution"] = {"alice_sr_units": float(alice), "bob_sr_units": float(bob)}

    # Separate collected income, fixed $10/day, no compounding: seven days late.
    junior_income = F(7) * 10 / 2 * F(10_000, 20_000)
    senior_income = F(7) * 10 / 2 * F(10_000, 90_000)
    assert junior_income == F(35, 2) and senior_income == F(35, 9)
    out["late_settlement_changes_rewards"] = {
        "junior_income_if_conversion_delayed": float(junior_income),
        "senior_income_if_converted_on_time": float(senior_income),
        "assumption": "Income intervals already known; production receivables not modeled."}

    notice, window = 7, 2
    cycle = notice + window
    eligible = lambda now, requested: requested + notice <= now < requested + cycle
    assert not eligible(6, 0) and eligible(7, 0) and eligible(8, 0)
    assert not eligible(9, 0) and not eligible(14, 0)
    assert not eligible(9, 9) and eligible(16, 9)
    counts = [sum((day - offset) % cycle >= notice for offset in range(cycle))
              for day in range(100)]
    assert min(counts) == max(counts) == window
    out["repeated_windows"] = {"notice_days": notice, "window_days": window,
                               "staggered_eligible_fraction": float(F(window, cycle)),
                               "assumptions": "Fixed delay, no fees/crowding, immediate renewal, half-open windows."}

    ledger = Ledger()
    prior = ledger.junior / 80_000
    ledger.convert(F(10_000))
    after = ledger.junior / 80_000
    assert prior == F(1, 4) and after == F(1, 8)
    out["coverage"] = {"before_and_during_notice": float(prior),
                       "after_conversion": float(after), "cash_unchanged": True}
    return {"scope": __doc__.strip(), "assertions_passed": True,
            "case_groups": len(out), "results": out}


if __name__ == "__main__":
    results = run()
    Path(__file__).with_name("examples.json").write_text(json.dumps(results, indent=2) + "\n")
    print(f"{results['case_groups']} accounting case groups passed; saved examples.json")
