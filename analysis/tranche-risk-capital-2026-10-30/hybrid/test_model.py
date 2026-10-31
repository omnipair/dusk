import unittest
import random
from dataclasses import replace
from model import (Params, Loan, initialize, mark, absorb_loss, request_exit,
                   settle_exits, coverage, accrue, liquidate, snapshot, run,
                   SCENARIOS, maximum_feasible, reset_wiped_epoch)


class AccountingTests(unittest.TestCase):
    def test_reserve_is_part_of_junior(self):
        for fraction in (0,.05,.1,.2):
            p = Params(reserve_fraction=fraction)
            s = initialize(p)
            self.assertAlmostEqual(s.active+s.reserve, p.capital)
            self.assertAlmostEqual(s.senior+s.junior, p.capital)
            snapshot(s,0,p)

    def test_reserve_draw_does_not_double_pay_loss(self):
        p = Params()
        s = initialize(p)
        s.loans[0].principal -= 1_000_000
        absorb_loss(s,1_000_000)
        self.assertEqual(s.junior,1_000_000)
        self.assertEqual(s.senior,8_000_000)
        self.assertEqual(s.reserve,0)
        self.assertEqual(s.totals["reserve_drawn"],500_000)
        snapshot(s,1,p)

    def test_junior_first_then_senior(self):
        p = Params()
        s = initialize(p)
        s.loans[0].principal -= 2_500_000
        absorb_loss(s,2_500_000)
        self.assertEqual(s.junior,0)
        self.assertEqual(s.senior,7_500_000)
        self.assertEqual(s.totals["senior_loss"],500_000)
        snapshot(s,1,p)

    def test_pending_remains_liable_and_paid_at_current_nav(self):
        p = Params(exit_policy="fixed")
        s = initialize(p)
        request_exit(s,0,.5,p)
        s.loans[0].principal -= 1_000_000
        absorb_loss(s,1_000_000)
        settle_exits(s,7,p)
        self.assertAlmostEqual(s.totals["withdrawn"],500_000)
        snapshot(s,7,p)

    def test_forward_capacity_excludes_requested_exits(self):
        p = Params()
        s = initialize(p)
        before = coverage(s,p)[0]
        request_exit(s,0,.5,p)
        self.assertLess(coverage(s,p)[0],before)
        self.assertAlmostEqual(coverage(s,p,forward=False)[0],before)

    def test_fixed_date_requires_actual_quote_cash(self):
        p = Params(exit_policy="fixed",reserve_fraction=0)
        s = initialize(p)
        # Reallocate quote to base; economic equity is unchanged at price 1.
        s.base += s.cash
        s.cash = 0
        request_exit(s,0,.5,p)
        settle_exits(s,7,p)
        self.assertEqual(s.totals["withdrawn"],0)
        self.assertGreater(s.pending,0)
        snapshot(s,7,p)

    def test_coverage_queue_cannot_promise_fixed_date(self):
        p = Params(voluntary_repayment=False,elasticity=0)
        rows = run(p,"exit_only")
        self.assertGreater(rows[-1]["pending_shares"],0)
        self.assertGreater(rows[-1]["overdue_shares"],0)

    def test_accrual_never_funds_junior_capital(self):
        p = Params()
        s = initialize(p)
        accrue(s,p)
        self.assertGreater(s.debt,p.initial_debt)
        self.assertEqual(s.junior,p.junior)
        self.assertEqual(s.totals["interest_collected"],0)
        snapshot(s,1,p)

    def test_rate_growth_can_cross_same_liquidation_threshold(self):
        p = Params(origination_ltv=.8499)
        s = initialize(p)
        accrue(s,p)
        liquidate(s,p,.1,rate_trigger=True)
        self.assertEqual(len(s.loans),0)
        self.assertGreater(s.totals["rate_liquidated_principal"],0)
        snapshot(s,1,p)

    def test_cash_interest_is_fully_distributed(self):
        p = Params(cash_interest_fraction=1)
        s = initialize(p)
        accrue(s,p)
        self.assertAlmostEqual(s.unpaid,0)
        self.assertAlmostEqual(s.totals["interest_collected"],
                               s.totals["junior_income"]+s.totals["senior_income"])
        snapshot(s,1,p)

    def test_admission_rejects_new_uncovered_debt(self):
        p = Params()
        s = initialize(p)
        request_exit(s,0,.9,p)
        amount = maximum_feasible(1_000_000,lambda a: coverage(s,p,new_loan=a)[0]>=p.target)
        self.assertEqual(amount,0)

    def test_marked_inventory_does_not_hit_segregated_quote(self):
        p = Params(reserve_fraction=.2)
        s = initialize(p)
        mark(s,.45)
        self.assertEqual(s.junior,p.junior)
        self.assertEqual(s.reserve,p.junior)
        snapshot(s,1,p)

    def test_junior_wipeout_clears_old_exit_entitlements(self):
        p = Params()
        s = initialize(p)
        request_exit(s,0,.5,p)
        s.loans[0].principal -= 2_000_000
        absorb_loss(s,2_000_000)
        self.assertEqual(s.pending,0)
        self.assertEqual(s.shares,0)
        self.assertEqual(s.totals["exit_lost_shares"],1_000_000)
        snapshot(s,1,p)

    def test_all_base_scenarios_conserve(self):
        for policy in ("fixed","coverage"):
            for scenario in SCENARIOS:
                rows = run(Params(exit_policy=policy),scenario)
                self.assertEqual(len(rows),91)

    def test_scalar_claim_burn_does_not_preserve_token_composition(self):
        # 200 quote-NAV, 100 senior/100 junior shares. Lose 50 quote.
        # Burning 50 junior shares preserves senior NAV, but changes its mix.
        senior_base = 100*100/150
        senior_quote = 100*50/150
        self.assertAlmostEqual(senior_base+senior_quote,100)
        self.assertNotAlmostEqual(senior_base,50)
        self.assertNotAlmostEqual(senior_quote,50)

    def test_randomized_loss_conservation_and_seniority(self):
        rng=random.Random(10429)
        for _ in range(200):
            p=Params(reserve_fraction=rng.uniform(0,.2))
            s=initialize(p)
            loss=rng.uniform(0,p.initial_debt)
            s.loans[0].principal-=loss
            absorb_loss(s,loss)
            self.assertAlmostEqual(s.senior,p.capital-p.junior-max(0,loss-p.junior))
            snapshot(s,1,p)

    def test_directional_insurance_requires_matching_currency(self):
        from two_asset import evaluate
        # Quote debt with a base-price crash: quote reserve protects more than base reserve.
        quote_cash=evaluate(2_500_000,0,1_000_000,0)[0]
        base_cash=evaluate(2_500_000,0,1_000_000,1)[0]
        self.assertGreater(quote_cash,base_cash)

    def test_two_asset_cash_is_not_fungible_without_conversion(self):
        from two_asset import evaluate
        # $3m segregated/active split leaves only $3m quote pool cash.
        self.assertEqual(evaluate(3_000_001,0,2_000_000,0)[1],"cash")


if __name__ == "__main__":
    unittest.main()
