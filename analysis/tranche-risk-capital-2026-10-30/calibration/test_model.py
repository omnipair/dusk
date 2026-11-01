import unittest
from dataclasses import replace
from math import exp, log
from model import Params, Scenario, Simulation, rate_step


class AccountingTests(unittest.TestCase):
    def test_interest_integration_partition(self):
        p=Params(cap=2)
        r,total=rate_step(.04,0,p,10)
        r1,first=rate_step(.04,0,p,3)
        r2,second=rate_step(r1,0,p,7)
        self.assertAlmostEqual(r,r2)
        self.assertAlmostEqual(total,first+second)

    def test_interest_cap_piecewise(self):
        p=Params(cap=.08)
        r,total=rate_step(.04,0,p,14)
        expected=(.08-.04)/(log(2)/7)+.08*7
        self.assertAlmostEqual(r,.08)
        self.assertAlmostEqual(total,expected)

    def test_senior_cash_exit_signal(self):
        m=Simulation(Params(),Scenario("test"))
        previous=m.score(0)
        m.cash_requests=100_000
        m.satisfy_cash_exits()
        self.assertLess(m.score(0),previous)
        self.assertAlmostEqual(m.j,200_000)
        m.check()

    def test_zero_junior_receives_funded_revenue(self):
        m=Simulation(Params(seed_fraction=0),Scenario("test",junior_fraction=0))
        m.pay_lp_revenue(1,100,m.ownership_split())
        self.assertEqual(m.j,50)
        self.assertEqual(m.seed_units,50)
        self.assertEqual(m.human_units,0)
        m.check()

    def test_pending_income_not_funded(self):
        m=Simulation(Params(),Scenario("test",junior_fraction=.001))
        old=m.j
        m.lots[1].append([100,*m.ownership_split()])
        self.assertEqual(m.j,old)
        m.repay(1,m.debt(1))
        self.assertAlmostEqual(m.j,old+45)

    def test_late_human_does_not_take_old_seed_income(self):
        m=Simulation(Params(),Scenario("test",junior_fraction=.001))
        m.lots[1].append([100,*m.ownership_split()])
        m.human_units+=100_000/m.unit_price()
        m.j+=100_000
        m.s-=100_000
        m.repay(1,m.debt(1))
        self.assertEqual(m.totals["human_junior_income"],0)
        self.assertAlmostEqual(m.totals["seed_income"],45)
        m.check()

    def test_liquidation_revenue_all_junior(self):
        m=Simulation(Params(),Scenario("test",junior_fraction=.001))
        m.pay_lp_revenue(0,100,m.ownership_split(junior_only=True))
        self.assertEqual(m.totals["seed_income"],100)
        self.assertEqual(m.totals["senior_income"],0)
        m.check()

    def test_queue_counts_until_conversion(self):
        m=Simulation(Params(),Scenario("test"))
        before=m.score(0)
        m.request_junior_exit(100_000)
        self.assertEqual(m.score(0),before)
        m.process_queue(10,False)
        self.assertLess(m.score(0),before)
        m.check()

    def test_insurance_asset_and_draw_cap(self):
        m=Simulation(Params(),Scenario("test"))
        other=m.insurance[0]
        m.credit_loss(1,.5)
        self.assertEqual(m.insurance[0],other)
        self.assertEqual(m.totals["insurance_used"],2000)
        m.check()

    def test_both_extreme_runs(self):
        for scenario in (Scenario("zero",junior_fraction=0),
                         Scenario("crash",shock_loss_fraction=.8,base_mark=.3)):
            m=Simulation(Params(days=45),scenario)
            m.run()
            m.check()


if __name__ == "__main__":
    unittest.main()
