from fractions import Fraction as F
import unittest
from accounting import Capital, Income, Exits


class ExactAccountingTests(unittest.TestCase):
    def market(self):
        c=Capital()
        c.deposit("creator",250_000,500_000)
        c.seed_from_creator("creator",F(1,1000))
        return c,Income(c)

    def test_seed_is_funded_and_initial_supply_conserves(self):
        c,_=self.market()
        self.assertEqual(c.nav,{"sr":999_000,"jr":1000})
        self.assertEqual(c.cash,[250_000,500_000])

    def test_seed_cannot_exit_or_transfer(self):
        c,_=self.market()
        with self.assertRaises(AssertionError):
            c.convert("seed","jr",1)
        with self.assertRaises(AssertionError):
            c.transfer("seed","creator","jr",1)

    def test_seed_receives_half_each_revenue_currency(self):
        c,i=self.market()
        for asset in (0,1):
            loan=f"borrow-{asset}"
            i.queue_accrual(loan,asset,100)
            i.prepare_page(loan)
            i.collect(loan,100)
        i.claim_page("seed",[0,1])
        i.claim_page("creator",[0,1])
        self.assertEqual(c.nav["jr"],1150)
        self.assertEqual(i.payout["creator",0],50)
        self.assertEqual(i.payout["creator",1],50)
        self.assertEqual(i.custody,[0,0])

    def test_later_user_does_not_get_seed_history(self):
        c,i=self.market()
        i.queue_accrual("loan",1,100)
        c.convert("creator","sr",100_000)
        i.prepare_page("loan")
        i.collect("loan",100)
        i.claim_page("seed",[0])
        i.claim_page("creator",[0])
        self.assertEqual(i.payout["creator",1],50)  # earlier senior half only
        self.assertEqual(c.nav["jr"],101_050)

    def test_former_insurance_fee_all_junior(self):
        c,i=self.market()
        old_insurance=c.insurance.copy()
        i.queue_accrual("liquidation_fee",0,100,junior_only=True)
        i.prepare_page("liquidation_fee")
        i.collect("liquidation_fee",100)
        i.claim_page("seed",[0])
        i.claim_page("creator",[0])
        self.assertEqual(c.nav["jr"],1200)
        self.assertEqual(i.payout["creator",0],0)
        self.assertEqual(c.insurance,old_insurance)

    def test_historical_owner_gets_income_after_transfer(self):
        c,i=self.market()
        i.queue_accrual("loan",1,100)
        c.transfer("creator","buyer","sr",c.shares("creator","sr"))
        i.prepare_page("loan")
        i.collect("loan",100)
        i.claim_page("buyer",[0])
        i.claim_page("creator",[0])
        self.assertEqual(i.payout["buyer",1],0)
        self.assertEqual(i.payout["creator",1],50)

    def test_partial_collection_then_new_interest(self):
        c,i=self.market()
        i.queue_accrual("loan",1,100)
        i.prepare_page("loan")
        i.collect("loan",40)
        i.claim_page("creator",[0])
        self.assertEqual(i.payout["creator",1],20)
        c.transfer("creator","buyer","sr",c.shares("creator","sr"))
        i.queue_accrual("loan",1,60)
        i.prepare_page("loan")
        i.collect("loan",120)
        i.claim_page("creator",[0,1])
        i.claim_page("buyer",[0,1])
        i.claim_page("seed",[0,1])
        self.assertEqual(i.payout["creator",1],50)
        self.assertEqual(i.payout["buyer",1],30)
        self.assertEqual(i.custody[1],0)

    def test_writeoff_does_not_reduce_funded_principal_twice(self):
        c,i=self.market()
        i.queue_accrual("loan",1,100)
        i.prepare_page("loan")
        i.collect("loan",20)
        old=c.nav.copy()
        i.writeoff("loan",80)
        self.assertEqual(c.nav,old)
        i.claim_page("creator",[0])
        self.assertEqual(i.payout["creator",1],10)

    def test_wiped_junior_old_tokens_cannot_claim_new_fees(self):
        c,i=self.market()
        c.convert("creator","sr",1000)
        old_token=c.tokens["jr"]
        c.lend(1,5000)
        c.loss(1,2000,0)
        self.assertEqual(c.nav["jr"],0)
        i.queue_accrual("post_wipe",1,100)
        i.prepare_page("post_wipe")
        i.collect("post_wipe",100)
        i.claim_page("creator",[0])
        i.claim_page("seed",[0])
        self.assertEqual(i.payout["creator",1],50)  # senior only
        self.assertNotEqual(c.tokens["jr"],old_token)
        self.assertEqual(c.shares("creator","jr"),0)
        self.assertEqual(c.nav["jr"],50)

    def test_one_junior_budget_for_both_assets(self):
        c,_=self.market()
        c.lend(0,1000)
        c.lend(1,1000)
        first=c.loss(0,400,0)
        second=c.loss(1,400,0)
        self.assertEqual(first,(0,800,0))
        self.assertEqual(second,(0,200,200))

    def test_insurance_is_actual_asset_specific(self):
        c,_=self.market()
        c.insurance=[F(100),F(0)]
        c.lend(1,2000)
        cover,jr,sr=c.loss(1,1500,10_000)
        self.assertEqual(cover,0)
        self.assertEqual(c.insurance[0],100)

    def test_pages_are_bounded_and_other_loan_not_blocked(self):
        c,i=self.market()
        for _ in range(25):
            i.queue_accrual("long_idle",1,10)
        i.queue_accrual("other",1,10)
        i.prepare_page("other")
        i.collect("other",10)
        self.assertEqual(i.prepare_page("long_idle"),8)
        with self.assertRaises(AssertionError):
            i.collect("long_idle",1)
        self.assertEqual(i.prepare_page("long_idle"),8)
        self.assertEqual(i.prepare_page("long_idle"),8)
        self.assertEqual(i.prepare_page("long_idle"),1)
        i.collect("long_idle",250)
        with self.assertRaises(AssertionError):
            i.claim_page("creator",list(range(9)))

    def test_duplicate_claim_does_not_pay_twice(self):
        c,i=self.market()
        i.queue_accrual("loan",1,100)
        i.prepare_page("loan")
        i.collect("loan",100)
        self.assertEqual(i.claim_page("creator",[0]),[0,50])
        self.assertEqual(i.claim_page("creator",[0]),[0,0])

    def test_hlp_funding_exclusion_keeps_exact_split(self):
        c,i=self.market()
        c.transfer("creator","hlp","sr",99_000)
        i.queue_accrual("hlp_funding",1,100,excluded_sr=("hlp",))
        i.prepare_page("hlp_funding")
        i.collect("hlp_funding",100)
        i.claim_page("hlp",[0])
        i.claim_page("creator",[0])
        i.claim_page("seed",[0])
        self.assertEqual(i.payout["hlp",1],0)
        self.assertEqual(i.payout["creator",1],50)
        self.assertEqual(i.custody[1],0)

    def test_internal_collateral_precedes_insurance(self):
        c,_=self.market()
        c.convert("creator","sr",19_000)
        c.transfer("creator","vault","sr",40_000)
        c.lend(1,50_000)
        c.insurance[1]=F(5000)
        paid=c.retire_internal_collateral("vault",40_000,1)
        self.assertEqual(paid,40_000)
        self.assertEqual(c.loss(1,10_000,5000),(5000,5000,0))
        self.assertEqual(c.shares("vault","sr"),0)

    def exit_market(self):
        c,i=self.market()
        c.convert("creator","sr",99_000)
        return c,i,Exits(c,5)

    def test_exit_fee_funds_seed_without_changing_total_junior(self):
        c,i,e=self.exit_market()
        old_nav=c.nav.copy()
        old_seed=c.shares("seed","jr")
        q=e.request("creator",10_000,F(0))
        self.assertEqual(q["fee"],5)
        self.assertEqual(c.shares("seed","jr"),old_seed+5)
        self.assertEqual(c.nav,old_nav)
        with self.assertRaises(AssertionError):
            c.transfer("creator","other","jr",c.shares("creator","jr"))

    def test_expiry_requires_fresh_notice(self):
        c,i,e=self.exit_market()
        q=e.request("creator",10_000,F(0))
        with self.assertRaises(AssertionError):
            e.claim("creator",q["ready"]+3)
        renewed=e.request("creator",0,q["ready"]+3)
        self.assertGreaterEqual(renewed["ready"],q["ready"]+10)
        self.assertGreater(renewed["fee"],0)

    def test_preserve_unused_window_through_pause(self):
        c,i,e=self.exit_market()
        q=e.request("creator",10_000,F(0))
        e.clock.begin(q["ready"]+1)
        with self.assertRaises(AssertionError):
            e.claim("creator",q["ready"]+5)
        e.clock.end(q["ready"]+5)
        e.claim("creator",q["ready"]+F(11,2))
        self.assertNotIn("creator",e.requests)

    def test_later_pause_does_not_revive_expired_window(self):
        c,i,e=self.exit_market()
        q=e.request("creator",10_000,F(0))
        e.clock.begin(q["ready"]+3)
        e.clock.end(q["ready"]+10)
        with self.assertRaises(AssertionError):
            e.claim("creator",q["ready"]+10)

    def test_added_request_cannot_shorten_existing_notice(self):
        c,i,e=self.exit_market()
        q=e.request("creator",10_000,F(0))
        q2=e.request("creator",1000,F(1))
        self.assertGreaterEqual(q2["ready"],q["ready"])
        self.assertEqual(q2["fee"],F(1,2))

    def test_restriction_beginning_during_notice(self):
        c,i,e=self.exit_market()
        q=e.request("creator",10_000,F(0))
        e.clock.begin(1)
        e.clock.end(q["ready"]+10)
        e.claim("creator",q["ready"]+11)


if __name__=="__main__":
    unittest.main()
