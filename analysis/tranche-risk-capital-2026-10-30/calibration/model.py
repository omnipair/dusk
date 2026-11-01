"""Synthetic two-token controller experiment; not Dusk's AMM or a forecast."""
from dataclasses import dataclass, replace
from math import exp, log, isfinite

EPS = 1e-8


@dataclass(frozen=True)
class Params:
    target: float = .05
    half_days: float = 7
    cap: float = 1
    borrow_limit: float = .8
    floor: float = .01
    initial_rate: float = .04
    band: float = .1
    dt: float = 1
    days: int = 90
    signal: str = "combined"
    queue_fee_bps: float = 0
    fee_destination: str = "seed"
    capital: float = 1_000_000
    seed_fraction: float = .001
    insurance_fraction: float = .02
    insurance_event_fraction: float = .20
    lp_interest_fraction: float = .9  # sensitivity assumption, not an inspected fee setting
    lp_revenue_signal: str = "forecast"


@dataclass(frozen=True)
class Scenario:
    name: str
    junior_fraction: float = .20
    utilization: tuple = (.60, .60)
    elasticity: float = 1
    borrower_days: float = 7
    junior_days: float = 7
    junior_hurdle: float = .15
    junior_enabled: bool = True
    turnover_daily: float = .03
    swap_fee_apr: float = .02  # net LP swap income / pool NAV; not a fee per trade
    senior_exit: float = 0
    junior_exit: float = 0
    shock_loss_fraction: float = 0
    base_mark: float = 1
    exit_day: float = 20
    shock_day: float = 30


SCENARIOS = [
    Scenario("mature"),
    Scenario("bootstrap", junior_fraction=.001),
    Scenario("bootstrap_slow", junior_fraction=.001, junior_days=30, junior_hurdle=.3),
    Scenario("seed_only", junior_fraction=.001, junior_enabled=False),
    Scenario("senior_run", senior_exit=.25),
    Scenario("junior_run", junior_exit=.75, junior_enabled=False),
    Scenario("inelastic", elasticity=0, senior_exit=.25),
    Scenario("revenue_drought", swap_fee_apr=0, turnover_daily=.005),
    Scenario("credit_stress", shock_loss_fraction=.30, base_mark=.65),
    Scenario("one_asset_scarce", utilization=(.85, .35)),
]


def rate_step(rate, score, p, dt):
    """Piecewise exact exponential integration while the input is unchanged."""
    direction = 1 if score < p.target * (1-p.band) else -1 if score > p.target * (1+p.band) else 0
    k = direction * log(2) / p.half_days
    if not k:
        return rate, rate * dt
    bound = p.cap if k > 0 else p.floor
    time_to_bound = log(bound / rate) / k if bound != rate else 0
    t = min(dt, max(0., time_to_bound))
    end = rate * exp(k*t)
    integral = (end-rate) / k + bound * (dt-t)
    return min(p.cap, max(p.floor, end)), integral


class Simulation:
    def __init__(self, p, scenario):
        self.p, self.sc = p, scenario
        self.price = [1., 1.]
        self.principal = [p.capital/2*u for u in scenario.utilization]
        self.cash = [p.capital/2-d for d in self.principal]
        self.insurance = [p.capital*p.insurance_fraction/2]*2
        self.j = p.capital * scenario.junior_fraction
        self.s = p.capital - self.j
        self.seed_units = min(self.j, p.capital*p.seed_fraction)
        self.human_units = self.j - self.seed_units
        self.rate = [p.initial_rate]*2
        # Each receivable carries immutable exposure-time revenue fractions.
        self.lots = [[], []]  # [gross unpaid, seed fraction, human junior fraction, senior fraction]
        self.queue = []  # [junior units, remaining notice, remaining unblocked window]
        self.cash_requests = 0.
        self.totals = dict.fromkeys(("borrow_value_days", "interest_accrued", "interest_collected",
            "seed_income", "human_junior_income", "senior_income", "junior_loss", "senior_loss",
            "insurance_used", "cash_requested", "cash_paid", "cash_wait_value_days", "origination",
            "queue_fee_value", "queued_units", "converted_value", "cap_days", "rate_turns"), 0.)
        self.history = []
        self.last_direction = [0, 0]
        self.block_remaining = 0.
        self.revenue_history = [(0.,0.)]

    def units(self):
        return self.seed_units + self.human_units

    def unit_price(self):
        return self.j / self.units() if self.units() > EPS else 1.

    def debt(self, a):
        return self.principal[a] + sum(x[0] for x in self.lots[a])

    def total_debt_value(self):
        return sum(self.price[a]*self.debt(a) for a in range(2))

    def utilization(self, a):
        d = self.debt(a)
        return d / (d+self.cash[a]) if d+self.cash[a] > EPS else 0.

    def score(self, a):
        if self.p.signal == "utilization":
            # Same time-based controller to isolate input, not native Morpho math.
            return self.p.target * (1-self.utilization(a)) / (1-.8)
        total_debt = self.total_debt_value()
        if total_debt <= EPS:
            return None  # no-debt bootstrap holds configured/current APR
        coverage = self.j / total_debt
        return coverage if self.p.signal == "coverage" else coverage*(1-self.utilization(a))

    def reset_wiped_junior(self):
        if self.j <= EPS:
            self.j = self.seed_units = self.human_units = 0.
            self.queue.clear()  # old generation principal is worthless; old income lots survive

    def seed_compound(self, asset, units):
        value = units*self.price[asset]
        if value <= EPS:
            return
        self.reset_wiped_junior()
        minted = value / self.unit_price()
        self.seed_units += minted
        self.j += value
        self.cash[asset] += units
        self.totals["seed_income"] += value

    def ownership_split(self, junior_only=False):
        jr_fraction = 1. if junior_only else .5
        if self.j > EPS:
            seed = jr_fraction*self.seed_units/self.units()
            human = jr_fraction-seed
        else:
            seed, human = jr_fraction, 0.
        return seed, human, 1-jr_fraction

    def pay_lp_revenue(self, asset, amount, split):
        seed, human, senior = split
        self.seed_compound(asset, amount*seed)
        self.totals["human_junior_income"] += amount*human*self.price[asset]
        self.totals["senior_income"] += amount*senior*self.price[asset]

    def repay(self, a, amount):
        debt = self.debt(a)
        if debt <= EPS or amount <= EPS:
            return
        amount = min(amount, debt)
        fraction = amount/debt
        principal_paid = self.principal[a]*fraction
        self.principal[a] -= principal_paid
        self.cash[a] += principal_paid
        for lot in self.lots[a]:
            interest_paid = lot[0]*fraction
            lot[0] -= interest_paid
            self.pay_lp_revenue(a, interest_paid*self.p.lp_interest_fraction, lot[1:])
            self.totals["interest_collected"] += interest_paid*self.price[a]
        self.lots[a] = [x for x in self.lots[a] if x[0] > EPS]

    def credit_loss(self, a, fraction):
        principal_lost = self.principal[a]*fraction
        self.principal[a] -= principal_lost
        for lot in self.lots[a]:
            lot[0] *= 1-fraction  # cancel unpaid income, not a second funded loss
        available = min(principal_lost, self.insurance[a]*self.p.insurance_event_fraction)
        self.insurance[a] -= available
        self.cash[a] += available
        remaining = (principal_lost-available)*self.price[a]
        jl = min(self.j, remaining)
        self.j -= jl
        self.s -= remaining-jl
        self.totals["junior_loss"] += jl
        self.totals["senior_loss"] += remaining-jl
        self.totals["insurance_used"] += available*self.price[a]
        self.reset_wiped_junior()

    def request_junior_exit(self, units):
        pending = sum(q[0] for q in self.queue)
        units = min(units, max(0., self.human_units-pending))
        if units <= EPS:
            return
        fee = units*self.p.queue_fee_bps/10_000
        self.totals["queue_fee_value"] += fee*self.unit_price()
        self.human_units -= fee
        if self.p.fee_destination == "seed":
            self.seed_units += fee  # actual transfer to a distinct, permanent holder
        # Otherwise burn units: value stays in J, rebating remaining holders.
        units -= fee
        notice = 7 + 3*min(1., (pending+units)/max(self.units(), EPS))
        self.queue.append([units, notice, 2.])
        self.totals["queued_units"] += units

    def process_queue(self, dt, blocked):
        keep = []
        for q in self.queue:
            q[1] -= dt
            if q[1] > EPS:
                keep.append(q)
            elif blocked:
                keep.append(q)  # exercise time does not expire while conversion blocked
            else:
                value = q[0]*self.unit_price()
                self.human_units -= q[0]
                self.j -= value
                self.s += value
                self.totals["converted_value"] += value
        self.queue = keep

    def satisfy_cash_exits(self):
        # Proportional basket redemption: both assets must be executable.
        nav = self.s+self.j
        fraction = min([1.] + [self.cash[a]/max(self.cash[a]+self.principal[a], EPS) for a in range(2)])
        value = min(self.cash_requests, self.s, nav*fraction)
        if value <= EPS:
            return
        for a in range(2):
            tokens = (self.cash[a]+self.principal[a])*value/nav
            self.cash[a] -= tokens
        self.s -= value
        self.cash_requests -= value
        self.totals["cash_paid"] += value

    def step(self, day):
        p, sc, dt = self.p, self.sc, self.p.dt
        if abs(day-sc.exit_day) < EPS:
            request = sc.senior_exit*p.capital
            self.cash_requests += request
            self.totals["cash_requested"] += request
            self.request_junior_exit(sc.junior_exit*self.human_units)
        if abs(day-sc.shock_day) < EPS:
            marked_loss = (1-sc.base_mark)*(self.cash[0]+self.principal[0])
            nav = self.s+self.j
            self.s *= (nav-marked_loss)/nav
            self.j *= (nav-marked_loss)/nav
            self.price[0] = sc.base_mark
            if sc.shock_loss_fraction:
                self.credit_loss(1, sc.shock_loss_fraction)
                self.block_remaining = 2
        if abs(day-45) < EPS:
            # Separate solvent-liquidation batch: fee recovered from its collateral,
            # not manufactured from the insolvent-loan shortfall on day 30.
            for debt_asset in range(2):
                collateral_asset = 1-debt_asset
                fee_value = .02*.1*self.principal[debt_asset]*self.price[debt_asset]
                self.pay_lp_revenue(collateral_asset, fee_value/self.price[collateral_asset],
                                    self.ownership_split(junior_only=True))
        blocked = self.block_remaining > EPS
        self.block_remaining = max(0., self.block_remaining-dt)
        for a in range(2):
            score = self.score(a)
            old_rate = self.rate[a]
            self.rate[a], integral = ((old_rate, old_rate*dt) if score is None else rate_step(old_rate, score, p, dt))
            change = (self.rate[a] > old_rate+EPS) - (self.rate[a] < old_rate-EPS)
            if change and self.last_direction[a] and change != self.last_direction[a]:
                self.totals["rate_turns"] += 1
            if change:
                self.last_direction[a] = change
            self.totals["cap_days"] += .5*dt*(self.rate[a] >= p.cap-EPS)
            debt = self.debt(a)
            interest = debt*integral/365
            self.lots[a].append([interest, *self.ownership_split()])
            self.totals["interest_accrued"] += interest*self.price[a]
            self.totals["borrow_value_days"] += self.principal[a]*self.price[a]*dt
        # Observations only; event ordering separates past accrual from new capital.
        self.process_queue(dt, blocked)
        self.satisfy_cash_exits()
        for a in range(2):
            swap = (self.s+self.j)*sc.swap_fee_apr*dt/365/2/self.price[a]
            self.pay_lp_revenue(a, swap, self.ownership_split())
            self.repay(a, self.debt(a)*(1-exp(-sc.turnover_daily*dt)))
            desired = p.capital/2*sc.utilization[a]*exp(max(-10., min(2., -sc.elasticity*(self.rate[a]-.08)/.08)))
            debt = self.debt(a)
            if debt > desired:
                self.repay(a, (debt-desired)*(1-exp(-dt/sc.borrower_days)))
            elif not self.cash_requests:
                demand = (desired-debt)*(1-exp(-dt/sc.borrower_days))
                headroom = max(0., p.borrow_limit*(debt+self.cash[a])-debt)
                borrowed = min(self.cash[a], demand, headroom)
                self.principal[a] += borrowed
                self.cash[a] -= borrowed
                self.totals["origination"] += borrowed*self.price[a]
        if sc.junior_enabled:
            forecast = sum(self.debt(a)*self.price[a]*self.rate[a] for a in range(2))*p.lp_interest_fraction
            forecast += (self.s+self.j)*sc.swap_fee_apr
            if p.lp_revenue_signal=="collected":
                cumulative=sum(self.totals[k] for k in ("seed_income","human_junior_income","senior_income"))
                eligible=[x for x in self.revenue_history if x[0]>=max(0.,day+dt-14)]
                start_time,start_amount=eligible[0]
                forecast=(cumulative-start_amount)*365/max(day+dt-start_time,dt)
            senior_apr = .5*forecast/max(self.s, 1.)
            desired_j = .5*forecast/(sc.junior_hurdle+senior_apr)
            delta = (desired_j-self.j)*(1-exp(-dt/sc.junior_days))
            if delta > 0:
                value = min(delta, self.s, .05*p.capital*dt)
                self.human_units += value/self.unit_price()
                self.j += value
                self.s -= value
            elif self.j > EPS:
                self.request_junior_exit(-delta/self.unit_price())
        self.satisfy_cash_exits()
        self.revenue_history.append((day+dt,sum(self.totals[k] for k in
            ("seed_income","human_junior_income","senior_income"))))
        self.totals["cash_wait_value_days"] += self.cash_requests*dt
        self.check()
        self.history.append(dict(day=day+dt, senior=self.s, junior=self.j,
            seed=self.seed_units*self.unit_price(), debt=self.total_debt_value(),
            rate_base=self.rate[0], rate_quote=self.rate[1], utilization_base=self.utilization(0),
            utilization_quote=self.utilization(1), cash_backlog=self.cash_requests))

    def check(self):
        actual = sum(self.price[a]*(self.cash[a]+self.principal[a]) for a in range(2))
        assert abs(actual-self.s-self.j) < .01, (actual, self.s, self.j)
        assert min(self.cash+self.principal+[self.s, self.j, self.human_units, self.seed_units]) > -.001
        assert all(isfinite(x) for x in self.rate+[self.s,self.j])
        assert sum(x[0] for x in self.queue) <= self.human_units+.001

    def run(self):
        for i in range(round(self.p.days/self.p.dt)):
            self.step(i*self.p.dt)
        t = self.totals
        return dict(scenario=self.sc.name, avg_borrow=t["borrow_value_days"]/self.p.days,
            borrower_apr=t["interest_accrued"]*365/max(t["borrow_value_days"],EPS),
            peak_apr=max(max(x["rate_base"],x["rate_quote"]) for x in self.history),
            junior_loss=t["junior_loss"], senior_loss=t["senior_loss"], insurance_used=t["insurance_used"],
            withdrawal_paid=t["cash_paid"], withdrawal_requested=t["cash_requested"],
            withdrawal_backlog=self.cash_requests, average_wait=t["cash_wait_value_days"]/max(t["cash_requested"],1),
            cap_days=t["cap_days"], rate_turns=t["rate_turns"],
            final_junior=self.j, final_seed=self.seed_units*self.unit_price(),
            junior_income=t["human_junior_income"], senior_income=t["senior_income"], seed_income=t["seed_income"],
            interest_accrued=t["interest_accrued"], interest_collected=t["interest_collected"],
            queue_fee_value=t["queue_fee_value"])
