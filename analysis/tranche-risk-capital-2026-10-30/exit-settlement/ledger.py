"""Reference ledger for tranche decisions, not deployable Dusk code.

Exact arithmetic in one valuation unit. Explicit reward-period allocations are
deliberately unbounded so economic ownership is inspectable. No AMM, oracle,
liquidation execution, token rounding, or Solana account-limit claims are made.
"""
from collections import defaultdict
from fractions import Fraction as F
from pathlib import Path
import json


class Ledger:
    def __init__(self, insurance=0):
        self.nav = {"sr": F(0), "jr": F(0)}
        self.balances = {"sr": defaultdict(F), "jr": defaultdict(F)}
        self.epoch = {"sr": 0, "jr": 0}
        self.retired = []
        self.insurance = F(insurance)
        self.principal_assets = F(0)
        self.periods = []
        self.paid_rewards = defaultdict(F)

    def supply(self, tier):
        return sum(self.balances[tier].values(), F(0))

    def value(self, who, tier):
        supply = self.supply(tier)
        return self.nav[tier] * self.balances[tier][who] / supply if supply else F(0)

    def mint(self, who, tier, value):
        value = F(value)
        assert value > 0
        supply = self.supply(tier)
        if self.nav[tier] == 0 and supply:
            # Conceptual epoch change. A deployed fungible token cannot simply
            # erase supply while old transferable tokens still exist.
            self.retired.append((tier, self.epoch[tier], dict(self.balances[tier])))
            self.epoch[tier] += 1
            self.balances[tier] = defaultdict(F)
            supply = F(0)
        minted = value * supply / self.nav[tier] if supply else value
        self.nav[tier] += value
        self.balances[tier][who] += minted
        return minted

    def deposit(self, who, tier, value):
        minted = self.mint(who, tier, value)
        self.principal_assets += value
        self.check()
        return minted

    def burn(self, who, tier, shares):
        shares = F(shares)
        assert 0 < shares <= self.balances[tier][who]
        value = shares * self.nav[tier] / self.supply(tier)
        self.balances[tier][who] -= shares
        self.nav[tier] -= value
        return value

    def convert(self, who, source, shares):
        target = "jr" if source == "sr" else "sr"
        total_before = self.principal_assets
        value = self.burn(who, source, shares)
        if value:
            self.mint(who, target, value)
        self.check()
        assert self.principal_assets == total_before
        return value

    def transfer(self, who, other, tier, shares):
        shares = F(shares)
        assert shares <= self.balances[tier][who]
        self.balances[tier][who] -= shares
        self.balances[tier][other] += shares
        self.check()

    def retire_default_collateral(self, who, senior_shares):
        # An internally held senior claim used to repay its own funding debt
        # extinguishes both an asset of the pool and a liability of the pool.
        value = self.burn(who, "sr", senior_shares)
        self.principal_assets -= value
        self.check()
        return value

    def credit_loss(self, residual_principal_loss):
        loss = F(residual_principal_loss)
        covered = min(loss, self.insurance)
        self.insurance -= covered
        left = loss - covered
        junior_loss = min(left, self.nav["jr"])
        senior_loss = left - junior_loss
        assert senior_loss <= self.nav["sr"]
        self.nav["jr"] -= junior_loss
        self.nav["sr"] -= senior_loss
        self.principal_assets -= left
        self.check()
        return {"insurance": covered, "junior": junior_loss, "senior": senior_loss}

    def accrue_net_lp_income(self, amount, excluded_senior=()):
        """Freeze conditional entitlements at exposure time, before collection.

        Candidate empty-tier rule: all to the other eligible tier. With neither
        eligible tier, use protocol insurance. Source-specific hLP exclusions can
        be supplied without treating gross hLP claims as new underwriting capital.
        This fallback is a proposed rule, not a user-approved product decision.
        """
        amount = F(amount)
        owners = {}
        for tier in ("sr", "jr"):
            owners[tier] = {who: shares for who, shares in self.balances[tier].items()
                            if shares > 0 and self.nav[tier] > 0
                            and not (tier == "sr" and who in excluded_senior)}
        eligible = [tier for tier in owners if owners[tier]]
        allocation = defaultdict(F)
        if not eligible:
            allocation["protocol_insurance"] = amount
        for tier in eligible:
            tier_amount = amount / len(eligible)
            supply = sum(owners[tier].values(), F(0))
            for who, shares in owners[tier].items():
                allocation[who] += tier_amount * shares / supply
        assert sum(allocation.values(), F(0)) == amount
        self.periods.append({"remaining": dict(allocation)})
        return len(self.periods) - 1

    def collect(self, period_id, amount):
        period = self.periods[period_id]["remaining"]
        total = sum(period.values(), F(0))
        amount = F(amount)
        assert 0 < amount <= total
        for who, entitlement in list(period.items()):
            collected = amount * entitlement / total
            period[who] -= collected
            self.paid_rewards[who] += collected
        self.check()

    def default_unpaid_income(self, period_id):
        period = self.periods[period_id]["remaining"]
        written_off = sum(period.values(), F(0))
        period.clear()
        # No extra funded-principal loss: this income was never booked there.
        self.check()
        return written_off

    def check(self):
        assert self.nav["sr"] + self.nav["jr"] == self.principal_assets
        for tier in self.nav:
            assert self.nav[tier] >= 0
            assert all(shares >= 0 for shares in self.balances[tier].values())
            assert sum((self.value(who, tier) for who in self.balances[tier]), F(0)) == self.nav[tier]


def run():
    results = {}
    x = Ledger()
    x.deposit("alice", "sr", 1000)
    x.convert("alice", "sr", 300)
    assert x.value("alice", "sr") == 700 and x.value("alice", "jr") == 300
    results["conversion"] = {"senior_value": 700, "junior_value": 300, "total_value": 1000}

    x = Ledger(insurance=5)
    x.deposit("senior", "sr", 80)
    x.deposit("junior", "jr", 20)
    loss = x.credit_loss(30)
    assert loss == {"insurance": 5, "junior": 20, "senior": 5}
    results["waterfall"] = loss

    # Generic leveraged srLP holder, not a recreation of Dusk's hLP engine.
    x = Ledger(insurance=5)
    x.deposit("ordinary", "sr", 60)
    x.deposit("leveraged_vault", "sr", 40)
    x.deposit("junior", "jr", 20)
    debt = F(50)
    recovered = x.retire_default_collateral("leveraged_vault", 40)
    loss = x.credit_loss(debt - recovered)
    assert loss == {"insurance": 5, "junior": 5, "senior": 0}
    assert x.value("leveraged_vault", "sr") == 0 and x.value("ordinary", "sr") == 60
    results["retire_defaulting_internal_claim_first"] = {"collateral_retired": recovered, "remaining_loss": loss}

    x = Ledger()
    x.deposit("senior", "sr", 80)
    x.deposit("early", "jr", 20)
    old_period = x.accrue_net_lp_income(10)
    x.collect(old_period, 4)
    x.convert("early", "jr", 20)
    x.deposit("late", "jr", 20)
    x.collect(old_period, 6)
    assert x.paid_rewards["early"] == 5 and x.paid_rewards["late"] == 0
    new_period = x.accrue_net_lp_income(10)
    x.collect(new_period, 10)
    assert x.paid_rewards["late"] == 5 and x.paid_rewards["early"] == 6
    results["partial_collection_and_switch"] = {
        "early_holder_old_junior_income": 5, "late_holder_old_income": 0,
        "late_holder_new_junior_income": 5, "early_holder_new_senior_income": 1}

    x = Ledger()
    x.deposit("alice", "sr", 100)
    old_period = x.accrue_net_lp_income(10)
    x.transfer("alice", "bob", "sr", 100)
    x.collect(old_period, 10)
    assert x.paid_rewards["alice"] == 10 and x.paid_rewards["bob"] == 0
    results["transfer_preserves_earned_income"] = True

    x = Ledger()
    x.deposit("senior", "sr", 100)
    old_period = x.accrue_net_lp_income(10)
    x.deposit("new_junior", "jr", 10)
    x.collect(old_period, 10)
    assert x.paid_rewards["senior"] == 10 and x.paid_rewards["new_junior"] == 0
    future_period = x.accrue_net_lp_income(10)
    x.collect(future_period, 10)
    assert x.paid_rewards["new_junior"] == 5
    results["empty_junior_fallback_candidate"] = {"old_income_to_senior": 10, "future_income_to_new_junior": 5}

    x = Ledger()
    x.deposit("senior", "sr", 100)
    period = x.accrue_net_lp_income(10)
    assert x.default_unpaid_income(period) == 10 and x.principal_assets == 100
    results["unpaid_income_writeoff_is_not_second_principal_loss"] = True

    x = Ledger()
    x.deposit("creator", "sr", 100_000)
    x.convert("creator", "sr", 100)  # actual funded 0.1%, not borrowed units
    x.transfer("creator", "permanent_seed", "jr", 100)
    period = x.accrue_net_lp_income(1000)
    x.collect(period, 1000)
    assert x.paid_rewards["permanent_seed"] == 500
    x.credit_loss(100)
    assert x.nav["jr"] == 0 and x.supply("jr") == 100
    x.convert("creator", "sr", 100)
    assert x.epoch["jr"] == 1 and x.value("permanent_seed", "jr") == 0
    results["permanent_seed"] = {"initial_junior": 100, "allocated_income": 500,
        "funded_loss_that_wipes_principal": 100,
        "note": "Rewards were distributed separately, not compounded. Compounded rewards can also be exhausted.",
        "recapitalization_requires_new_economic_generation": True}

    x = Ledger()
    x.deposit("junior", "jr", 20)
    p = x.accrue_net_lp_income(10)
    x.collect(p, 10)
    assert x.paid_rewards["junior"] == 10
    results["empty_senior_fallback_candidate"] = True

    # Both ratios are dimensionless and unchanged if all dollar amounts scale.
    def signals(j, d, c):
        total = d + c
        u = F(d, total)
        return {"junior_pool_share": F(j, total), "utilization": u,
                "coverage": F(j, d), "raw_J_over_U": F(j) / u,
                "combined_candidate": F(j, d) * (1 - u)}
    before, after = signals(20, 80, 20), signals(20, 80, 10)
    scaled = signals(200, 800, 200)
    assert before["coverage"] == after["coverage"] == F(1, 4)
    assert after["combined_candidate"] < before["combined_candidate"]
    assert scaled["combined_candidate"] == before["combined_candidate"]
    assert scaled["raw_J_over_U"] == 10 * before["raw_J_over_U"]
    results["senior_withdrawal_signals"] = {"before": before, "after": after, "scaled_pool": scaled}

    # Counting 8/20 pending capital while it remains slashable changes no capacity.
    results["pending_notice"] = {"actual_coverage": F(20, 80), "if_immediately_excluded": F(12, 80),
        "physical_first_loss_capacity_in_either_case": 20,
        "interpretation": "Exclusion creates a forward signal; it does not measure current capacity."}
    return {"scope": __doc__.strip(), "case_groups": len(results), "checks_passed": True, "results": results}


if __name__ == "__main__":
    results = run()
    Path(__file__).with_name("ledger-results.json").write_text(
        json.dumps(results, default=lambda x: float(x) if isinstance(x, F) else str(x), indent=2) + "\n")
    print(f"{results['case_groups']} reference-ledger case groups passed")
