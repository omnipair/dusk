import json
import unittest
from pathlib import Path

from model import (POLICIES, SCENARIOS, Scenario, Shock, apply_shock, check_state,
                   enter_junior, income_and_borrowing, initialize, liquidatable,
                   recovery_fraction, request_exit, settle_exits, simulate, update_rate)

P = json.loads(Path(__file__).with_name("config.json").read_text())


class AccountingTests(unittest.TestCase):
    def test_equal_initial_capital_debt_and_apr(self):
        for policy in POLICIES:
            s = initialize(policy, P)
            self.assertEqual(s.active + s.reserve, 10_000_000)
            self.assertEqual(s.debt, 5_600_000)
            self.assertEqual(s.apr, .08)
            check_state(s, P)

    def test_loss_waterfall_and_cash_distinction(self):
        shock = Shock(1, .45, .20, .75, 0)
        j = initialize("junior_fixed", P)
        r = initialize("reserve_uncapped", P)
        apply_shock(j, shock, P)
        apply_shock(r, shock, P)
        expected = 5_600_000 * .75 * (1 - .45 / .7 * .8)
        self.assertAlmostEqual(j.totals["credit_loss"], expected)
        self.assertAlmostEqual(j.totals["junior_credit_loss"], 2_000_000)
        self.assertAlmostEqual(j.totals["senior_credit_loss"], expected - 2_000_000)
        self.assertAlmostEqual(r.totals["insurance_drawn"], 2_000_000)
        self.assertAlmostEqual(j.cash, r.cash)
        check_state(j, P)
        check_state(r, P)

    def test_event_cap_and_daily_cap(self):
        shock = Shock(1, .40, .30, .9, 0)
        for events, expected in ((1, 400_000), (20, 1_000_000)):
            p = {**P, "liquidations_per_shock": events}
            s = initialize("reserve_capped", p)
            apply_shock(s, shock, p)
            self.assertAlmostEqual(s.totals["insurance_drawn"], expected)
            # A later batch that same day cannot replenish the daily allowance.
            before = s.totals["insurance_drawn"]
            apply_shock(s, shock, p)
            self.assertLessEqual(s.totals["insurance_drawn"], 1_000_000 + 1e-5)
            if events == 20:
                self.assertAlmostEqual(s.totals["insurance_drawn"], before)
            check_state(s, p)

    def test_execution_depth_changes_recovery_not_trigger(self):
        for discount in (0.0, .1, .5):
            self.assertTrue(liquidatable(.8, P))
            self.assertFalse(liquidatable(.9, P))
            self.assertLessEqual(recovery_fraction(.8, discount, P), 1.0)
        self.assertGreater(recovery_fraction(.8, .1, P), recovery_fraction(.8, .5, P))

    def test_queued_exit_remains_loss_exposed(self):
        s = initialize("junior_fixed", P)
        request_exit(s, 1_000_000, 0, P, cash=True)
        self.assertEqual(s.committed_junior, 1_000_000)
        settle_exits(s, 6)
        self.assertEqual(s.totals["withdrawn"], 0)
        apply_shock(s, Shock(7, .6, .1, .5, 0), P)
        expected = .5 * s.junior
        settle_exits(s, 7)
        self.assertAlmostEqual(s.totals["withdrawn"], expected)
        self.assertLess(s.totals["withdrawn"], 1_000_000)
        check_state(s, P)

    def test_cash_exit_waits_but_claim_switch_can_settle(self):
        for cash in (True, False):
            s = initialize("junior_fixed", P)
            s.debt += s.cash
            s.cash = 0
            request_exit(s, 1_000_000, 0, P, cash=cash)
            settle_exits(s, 7)
            if cash:
                self.assertAlmostEqual(s.pending_value, 1_000_000)
                self.assertEqual(s.totals["withdrawn"], 0)
            else:
                self.assertEqual(s.pending_value, 0)
                self.assertEqual(s.senior, 9_000_000)
            check_state(s, P)

    def test_new_junior_epoch_does_not_revive_old_shares(self):
        s = initialize("junior_fixed", P)
        request_exit(s, 1_000_000, 0, P, cash=True)
        apply_shock(s, Shock(1, .4, .3, .9, 0), P)
        self.assertEqual(s.junior_shares, 0)
        self.assertEqual(s.pending_value, 0)
        enter_junior(s, 100_000)
        self.assertEqual(s.junior_shares, 100_000)
        settle_exits(s, 30)
        self.assertEqual(s.junior, 100_000)
        self.assertEqual(s.totals["withdrawn"], 0)
        check_state(s, P)

    def test_inventory_loss_is_shared_before_credit(self):
        s = initialize("junior_fixed", P)
        apply_shock(s, Shock(1, 1.0, 0, 0, .25), P)
        self.assertAlmostEqual(s.junior, 2_000_000 - 4_400_000 * .25 * .2)
        self.assertEqual(s.totals["credit_loss"], 0)
        check_state(s, P)

    def test_zero_junior_does_not_receive_income(self):
        s = initialize("junior_fixed", P)
        request_exit(s, s.junior, 0, {**P, "exit_notice_days": 0})
        settle_exits(s, 0)
        income_and_borrowing(s, 0, Scenario("empty_junior"), P)
        self.assertEqual(s.totals["junior_income"], 0)
        check_state(s, P)

    def test_zero_debt_and_zero_junior_have_finite_rates(self):
        for policy in ("junior_share", "junior_debt"):
            s = initialize(policy, P)
            s.cash += s.debt
            s.debt = 0
            request_exit(s, s.junior, 0, {**P, "exit_notice_days": 0})
            settle_exits(s, 0)
            update_rate(s, P)
            self.assertTrue(P["min_apr"] <= s.apr <= P["max_apr"])
            check_state(s, P)

    def test_more_debt_is_visible_only_to_debt_controller(self):
        for policy in ("junior_share", "junior_debt"):
            s = initialize(policy, P)
            s.debt += 1_000_000
            s.cash -= 1_000_000
            update_rate(s, P)
            if policy == "junior_share":
                self.assertEqual(s.apr, .08)
            else:
                self.assertGreater(s.apr, .08)

    def test_full_scenario_grid_conserves_balances(self):
        for scenario in SCENARIOS:
            for policy in POLICIES:
                summary, _, _ = simulate(policy, scenario, P)
                self.assertGreaterEqual(summary["total_capital_pnl"], -P["initial_capital"])


if __name__ == "__main__":
    unittest.main()
