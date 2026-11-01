import unittest
import experiment as e


class Contracts(unittest.TestCase):
    def test_full_cap_admits_no_additional_borrowing(self):
        self.assertEqual(e.borrow_capacity(80,20,.8),0)

    def test_post_borrow_cap_not_just_pre_borrow_check(self):
        self.assertAlmostEqual(e.borrow_capacity(70,30,.8),10)

    def test_senior_withdrawal_leaves_junior_debt_signal_unchanged(self):
        p,s=e.start({})
        ratio=s.junior/s.debt
        s.cash-=1_000_000;s.senior-=1_000_000
        self.assertEqual(s.junior/s.debt,ratio)
        self.assertGreater(s.debt/(s.debt+s.cash),.8)
        self.assertEqual(e.borrow_capacity(s.debt,s.cash,.8),0)

    def test_junior_conversion_needs_no_cash(self):
        p,s=e.start({})
        s.senior-=s.cash;s.cash=0
        debt=s.debt
        e.m.request_exit(s,0,.5,p)
        e.convert_ready(s,7)
        self.assertEqual(s.cash,0)
        self.assertEqual(s.debt,debt)
        self.assertEqual(s.junior,1_000_000)
        self.assertEqual(s.pending,0)

    def test_conversion_stays_exposed_to_prior_losses(self):
        p,s=e.start({})
        e.m.request_exit(s,0,.5,p)
        s.loans[0].principal-=1_000_000
        e.m.absorb_loss(s,1_000_000)
        e.convert_ready(s,7)
        self.assertEqual(s.totals["converted_value"],500_000)

    def test_interest_can_cross_cap_without_new_borrow(self):
        p,s=e.start({})
        e.m.accrue(s,p)
        self.assertGreater(s.debt/(s.debt+s.cash),.8)
        self.assertEqual(e.borrow_capacity(s.debt,s.cash,.8),0)
        self.assertEqual(s.totals["originated"],0)

    def test_risk_price_does_not_take_cash_or_utilization(self):
        self.assertEqual(e.risk_step(.08,20,80,7),.08)
        self.assertGreater(e.risk_step(.08,10,80,7),.08)
        self.assertLess(e.risk_step(.08,20,40,7),.08)


if __name__=="__main__":unittest.main()
