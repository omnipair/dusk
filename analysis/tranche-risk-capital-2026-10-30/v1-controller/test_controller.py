from dataclasses import replace
import unittest
from controller import RateModel,MarketController,DAY_MS,band_at_reference_utilization


class ControllerContractTests(unittest.TestCase):
    def test_pool_allocation_maps_to_fixed_signal_band(self):
        low,high=band_at_reference_utilization(.2,.25,.8)
        self.assertAlmostEqual(low,.05)
        self.assertAlmostEqual(high,.0625)

    def test_economic_defaults_and_inclusive_band(self):
        p=RateModel()
        self.assertEqual((p.initial_rate,p.min_rate,p.max_rate),(.04,.02,0))
        self.assertEqual(p.half_life_ms,14*DAY_MS)
        for score in (.05,.056,.0625):
            rate,area=p.integrate(.04,score,DAY_MS)
            self.assertEqual(rate,.04)
            self.assertEqual(area,.04*DAY_MS)

    def test_low_coverage_raises_and_high_coverage_lowers(self):
        p=RateModel()
        self.assertAlmostEqual(p.integrate(.04,.03,p.half_life_ms)[0],.08)
        self.assertAlmostEqual(p.integrate(.04,.08,p.half_life_ms)[0],.02)
        self.assertAlmostEqual(p.integrate(.04,.08,3*p.half_life_ms)[0],.02)

    def test_uncapped_rate_crosses_old_50_percent_ceiling(self):
        p=RateModel()
        self.assertAlmostEqual(p.integrate(.04,0,5*p.half_life_ms)[0],1.28)

    def test_long_half_lives_do_not_quantize_to_zero(self):
        for days in (1,3,7,14,30):
            p=replace(RateModel(),half_life_ms=days*DAY_MS)
            self.assertAlmostEqual(p.integrate(.04,0,p.half_life_ms)[0],.08)

    def test_partition_preserves_elapsed_interest(self):
        p=RateModel()
        for score in (0,.056,.5):
            rate,whole=p.integrate(.04,score,9*DAY_MS)
            part,first=p.integrate(.04,score,2*DAY_MS)
            last,second=p.integrate(part,score,7*DAY_MS)
            self.assertAlmostEqual(rate,last)
            self.assertAlmostEqual(whole,first+second,places=7)

    def test_governance_update_settles_old_interval_without_reset(self):
        old=RateModel()
        market=MarketController("governance",old,signals=(0,.056))
        new=replace(old,half_life_ms=DAY_MS,initial_rate=.06)
        market.set_model("governance",new,old.half_life_ms)
        self.assertAlmostEqual(market.rates[0],.08)
        self.assertAlmostEqual(market.integrals[0],old.integrate(.04,0,old.half_life_ms)[1])
        market.accrue(old.half_life_ms+DAY_MS)
        self.assertAlmostEqual(market.rates[0],.16)
        self.assertAlmostEqual(market.rates[1],.04)

    def test_unauthorized_update_does_not_accrue_or_mutate(self):
        old=RateModel()
        market=MarketController("governance",old,signals=(0,0))
        with self.assertRaises(AssertionError):
            market.set_model("stranger",replace(old,half_life_ms=DAY_MS),DAY_MS)
        self.assertIs(market.model,old)
        self.assertEqual(market.last_ms,0)

    def test_state_change_does_not_reprice_previous_interval(self):
        p=RateModel()
        market=MarketController("governance",p,signals=(0,0))
        market.set_signals((1,1),p.half_life_ms)
        self.assertAlmostEqual(market.rates[0],.08)
        market.accrue(2*p.half_life_ms)
        self.assertAlmostEqual(market.rates[0],.04)

    def test_optional_apr_inputs_override_defaults_and_preserve_zero(self):
        defaults=RateModel.from_overrides(initial_rate=None,min_rate=None,max_rate=None)
        self.assertEqual(defaults,RateModel())
        supplied=RateModel.from_overrides(initial_rate=.06,min_rate=.03,max_rate=.5,half_life_ms=None)
        market=MarketController("gov",supplied,signals=(0,1))
        market.accrue(5*supplied.half_life_ms)
        self.assertAlmostEqual(market.rates[0],.5)
        self.assertAlmostEqual(market.rates[1],.03)
        uncapped=RateModel.from_overrides(initial_rate=.06,min_rate=0,max_rate=0)
        self.assertEqual(uncapped.min_rate,0)
        self.assertGreater(uncapped.integrate(.06,0,5*uncapped.half_life_ms)[0],.5)

    def test_models_are_market_specific(self):
        first=MarketController("gov",RateModel(),signals=(0,0))
        second=MarketController("gov",replace(RateModel(),half_life_ms=DAY_MS),signals=(0,0))
        first.accrue(DAY_MS)
        second.accrue(DAY_MS)
        self.assertLess(first.rates[0],second.rates[0])


if __name__=="__main__":unittest.main()
