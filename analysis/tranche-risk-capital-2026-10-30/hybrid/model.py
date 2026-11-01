"""Synthetic two-asset balance sheet; not a native AMM or calibrated market."""
from dataclasses import dataclass, field, replace
from math import exp, log

EPS = 1e-7


@dataclass(frozen=True)
class Params:
    capital: float = 10_000_000
    junior: float = 2_000_000
    initial_debt: float = 3_000_000
    base_units: float = 2_000_000
    reserve_fraction: float = .05
    target: float = 1.10
    funding: float = .04
    initial_premium: float = .04
    min_premium: float = .001
    max_premium: float = .25
    half_life: float = 7
    notice: float = 7
    elasticity: float = 1
    response_days: float = 7
    cash_interest_fraction: float = 0
    voluntary_repayment: bool = True
    admission: bool = True
    exit_policy: str = "coverage"
    dt: float = 1
    days: float = 90
    origination_ltv: float = .70
    liquidation_ltv: float = .85
    stresses: tuple = ((.60, .10), (.45, .20))


@dataclass
class Loan:
    principal: float
    collateral: float
    funding_due: float = 0
    premium_due: float = 0

    @property
    def due(self):
        return self.principal + self.funding_due + self.premium_due


@dataclass
class Exit:
    shares: float
    ready: float


@dataclass
class State:
    cash: float
    base: float
    reserve: float
    senior: float
    junior: float
    shares: float
    premium: float
    price: float = 1
    loans: list = field(default_factory=list)
    exits: list = field(default_factory=list)
    totals: dict = field(default_factory=lambda: dict.fromkeys((
        "inventory_loss", "principal_loss", "junior_loss", "senior_loss",
        "withdrawn", "reserve_drawn", "interest_accrued", "interest_collected",
        "interest_written_off", "junior_income", "senior_income", "originated",
        "principal_recovered", "liquidated_principal", "rate_liquidated_principal",
        "exit_requested", "exit_paid_shares", "exit_lost_shares",
        "exit_delay_share_days", "blocked_new_debt", "new_capital"), 0.0))

    @property
    def principal(self):
        return sum(x.principal for x in self.loans)

    @property
    def unpaid(self):
        return sum(x.funding_due + x.premium_due for x in self.loans)

    @property
    def debt(self):
        return self.principal + self.unpaid

    @property
    def active(self):
        return self.senior + self.junior - self.reserve

    @property
    def nav(self):
        return self.junior / self.shares if self.shares > EPS else 1

    @property
    def pending(self):
        return sum(x.shares for x in self.exits)


def initialize(p):
    reserve = p.capital * p.reserve_fraction
    assert 0 <= reserve <= p.junior
    return State(p.capital - p.initial_debt - p.base_units - reserve,
                 p.base_units, reserve, p.capital-p.junior, p.junior, p.junior,
                 p.initial_premium,
                 loans=[Loan(p.initial_debt, p.initial_debt/p.origination_ltv)])


def liquidatable(loan, price, p):
    return loan.due >= loan.collateral * price * p.liquidation_ltv - EPS


def credit_loss(loan, price, discount, p):
    if not liquidatable(loan, price, p):
        return 0.0
    recovery = min(loan.due, loan.collateral * price * (1-discount))
    principal_paid = max(0.0, recovery - loan.funding_due - loan.premium_due)
    return max(0.0, loan.principal-principal_paid)


def coverage(st, p, *, forward=True, exit_amount=0.0, new_loan=0.0):
    """Evaluate post-action coverage without mutating state; reserve belongs to J."""
    if st.junior <= EPS:
        remaining_fraction = 0.0
    else:
        remaining_fraction = max(0.0, 1-exit_amount/st.junior)
        if forward:
            remaining_fraction = min(remaining_fraction,
                                     max(0.0, 1-st.pending/max(st.shares, EPS)))
    r = st.reserve*remaining_fraction
    j = st.junior*remaining_fraction
    active = st.senior+j-r
    # Cash exits remove quote cash; base exposure stays in the remaining pool.
    q = max(0.0, j-r)/active if active > EPS else 0
    stress_rows = []
    for gap, discount in p.stresses:
        px = st.price*gap
        inventory_loss = st.base*(st.price-px)
        junior_stress = max(0.0, r+q*(active-inventory_loss))
        loss = sum(credit_loss(x, px, discount, p) for x in st.loans)
        if new_loan > 0:
            loan = Loan(new_loan, new_loan/(p.origination_ltv*st.price))
            loss += credit_loss(loan, px, discount, p)
        ratio = junior_stress/loss if loss > EPS else 1e6
        stress_rows.append((ratio, junior_stress, loss))
    return min(stress_rows, key=lambda x: x[0])


def reset_wiped_epoch(st):
    if st.junior <= EPS:
        st.junior = st.reserve = 0.0
        st.totals["exit_lost_shares"] += st.pending
        st.exits.clear()
        st.shares = 0.0


def mark(st, new_price):
    loss = st.base*(st.price-new_price)
    q = max(0.0, st.junior-st.reserve)/st.active if st.active > EPS else 0
    st.junior -= q*loss
    st.senior -= (1-q)*loss
    st.price = new_price
    st.totals["inventory_loss"] += loss
    reset_wiped_epoch(st)


def distribute_interest(st, funding, premium):
    q = max(0.0, st.junior-st.reserve)/st.active if st.active > EPS else 0
    # If J is exhausted, no phantom premium entitlement survives that epoch.
    junior_income = funding*q + (premium if st.junior > EPS else 0)
    total = funding+premium
    st.totals["junior_income"] += junior_income
    st.totals["senior_income"] += total-junior_income
    st.totals["interest_collected"] += total


def collect(st, loan, cash):
    cash = min(cash, loan.due)
    interest = loan.funding_due+loan.premium_due
    paid_interest = min(cash, interest)
    f = paid_interest*loan.funding_due/interest if interest > EPS else 0
    prem = paid_interest-f
    loan.funding_due = max(0.0, loan.funding_due-f)
    loan.premium_due = max(0.0, loan.premium_due-prem)
    distribute_interest(st, f, prem)
    principal = min(loan.principal, cash-paid_interest)
    loan.principal -= principal
    st.cash += principal
    st.totals["principal_recovered"] += principal


def absorb_loss(st, loss):
    assert loss <= st.senior+st.junior+EPS
    draw = min(loss, st.reserve)
    st.reserve -= draw
    st.cash += draw
    junior_loss = min(loss, st.junior)
    st.junior -= junior_loss
    st.senior -= loss-junior_loss
    st.totals["principal_loss"] += loss
    st.totals["junior_loss"] += junior_loss
    st.totals["senior_loss"] += loss-junior_loss
    st.totals["reserve_drawn"] += draw
    reset_wiped_epoch(st)


def liquidate(st, p, discount, rate_trigger=False):
    survivors = []
    for loan in st.loans:
        if not liquidatable(loan, st.price, p):
            survivors.append(loan)
            continue
        st.totals["liquidated_principal"] += loan.principal
        if rate_trigger:
            st.totals["rate_liquidated_principal"] += loan.principal
        collect(st, loan, min(loan.due, loan.collateral*st.price*(1-discount)))
        st.totals["interest_written_off"] += loan.funding_due+loan.premium_due
        absorb_loss(st, loan.principal)
    st.loans = survivors


def request_exit(st, day, fraction, p):
    shares = (st.shares-st.pending)*fraction
    st.exits.append(Exit(shares, day+p.notice))
    st.totals["exit_requested"] += shares*st.nav


def maximum_feasible(upper, predicate):
    if upper <= EPS or not predicate(0):
        return 0.0
    if predicate(upper):
        return upper
    lo, hi = 0.0, upper
    for _ in range(42):
        mid = (lo+hi)/2
        if predicate(mid):
            lo = mid
        else:
            hi = mid
    return lo


def settle_exits(st, day, p):
    pending = []
    for req in st.exits:
        if req.ready > day+EPS:
            pending.append(req)
            continue
        nav = st.nav
        wanted = min(st.junior, req.shares*nav)
        # Withdraw proportionally from segregated reserve and active junior claim.
        cash_fraction = 1-st.reserve/st.junior if st.junior > EPS else 1
        upper = min(wanted, st.cash/cash_fraction) if cash_fraction > EPS else wanted
        if p.exit_policy == "coverage":
            paid = maximum_feasible(upper, lambda a: coverage(
                st, p, forward=False, exit_amount=a)[0] >= p.target-EPS)
        else:
            paid = upper
        paid = max(0.0, paid)
        shares = paid/nav if nav > EPS else 0.0
        reserve_share = paid*st.reserve/st.junior if st.junior > EPS else 0
        st.reserve -= reserve_share
        st.cash -= paid-reserve_share
        st.junior -= paid
        st.shares -= shares
        st.totals["withdrawn"] += paid
        st.totals["exit_paid_shares"] += shares
        st.totals["exit_delay_share_days"] += shares*max(0.0, day-req.ready)
        if req.shares-shares > EPS:
            pending.append(Exit(req.shares-shares, req.ready))
    st.exits = pending


def accrue(st, p):
    for loan in st.loans:
        basis = loan.due
        f = basis*p.funding*p.dt/365
        prem = basis*st.premium*p.dt/365
        loan.funding_due += f
        loan.premium_due += prem
        st.totals["interest_accrued"] += f+prem
        collect(st, loan, (f+prem)*p.cash_interest_fraction)


def adjust_debt(st, day, growth, p):
    apr = p.funding+st.premium
    target = min(8_000_000, p.initial_debt*growth*(apr/.08)**(-p.elasticity))
    desired = (target-st.debt)*(1-exp(-p.dt/p.response_days))
    if desired < 0 and p.voluntary_repayment and st.debt > EPS:
        fraction = min(1, -desired/st.debt)
        for loan in st.loans:
            # A pro-rata subposition closes, releasing the same collateral share.
            part = Loan(loan.principal*fraction, loan.collateral*fraction,
                        loan.funding_due*fraction, loan.premium_due*fraction)
            loan.principal *= 1-fraction
            loan.collateral *= 1-fraction
            loan.funding_due *= 1-fraction
            loan.premium_due *= 1-fraction
            collect(st, part, part.due)
    elif desired > 0:
        upper = min(desired, st.cash)
        admitted = upper
        if p.admission:
            admitted = maximum_feasible(upper, lambda a: coverage(st, p, new_loan=a)[0] >= p.target-EPS)
        st.totals["blocked_new_debt"] += desired-admitted
        if admitted > EPS:
            st.loans.append(Loan(admitted, admitted/(p.origination_ltv*st.price)))
            st.cash -= admitted
            st.totals["originated"] += admitted


def update_rate(st, p):
    ratio = coverage(st, p)[0]
    err = max(-1, min(2, 1-ratio/p.target))
    if abs(err) < .05:
        err = 0
    st.premium = min(p.max_premium, max(p.min_premium,
        st.premium*exp(log(2)/p.half_life*err*p.dt)))


SCENARIOS = {
    "calm": {},
    "growth": {"growth_day":20, "growth":2},
    "exit_only": {"exit_day":30},
    "exit_crash": {"exit_day":30, "crash_day":40, "price":.45},
    "growth_crash": {"growth_day":20, "growth":2, "crash_day":40, "price":.45},
    "exit_tail": {"exit_day":30, "crash_day":40, "price":.30},
}


def snapshot(st, day, p):
    a = st.cash+st.base*st.price+st.principal
    residual = a-(st.senior+st.junior-st.reserve)
    conservation = (a+st.reserve+st.totals["withdrawn"]+st.totals["inventory_loss"]
                    +st.totals["principal_loss"]-p.capital-st.totals["new_capital"])
    assert abs(residual) < .001, (day, residual)
    assert abs(conservation) < .001, (day, conservation)
    assert min(st.cash, st.senior, st.junior, st.reserve) >= -.001
    assert st.reserve <= st.junior+.001
    assert st.pending <= st.shares+.001
    return dict(day=day, cash=st.cash, base=st.base, price=st.price, principal=st.principal,
                unpaid=st.unpaid, debt=st.debt, senior=st.senior, junior=st.junior,
                reserve=st.reserve, active=st.active, shares=st.shares, pending_shares=st.pending,
                pending_value=st.pending*st.nav, apr=p.funding+st.premium,
                forward_coverage=min(10,coverage(st,p)[0]),
                actual_coverage=min(10,coverage(st,p,forward=False)[0]),
                overdue_shares=sum(x.shares for x in st.exits if x.ready <= day+EPS),
                **st.totals)


def run(p, scenario):
    c = SCENARIOS[scenario]
    st = initialize(p)
    rows = [snapshot(st,0,p)]
    for n in range(1, round(p.days/p.dt)+1):
        day = n*p.dt
        if abs(day-c.get("crash_day",-1)) < EPS:
            mark(st, c["price"])
        liquidate(st,p,.20)
        if abs(day-c.get("exit_day",-1)) < EPS:
            request_exit(st,day,.5,p)
        settle_exits(st,day,p)
        update_rate(st,p)
        accrue(st,p)
        growth = c.get("growth",1) if day >= c.get("growth_day",1e6) else 1
        adjust_debt(st,day,growth,p)
        liquidate(st,p,.10,rate_trigger=True)
        settle_exits(st,day,p)
        rows.append(snapshot(st,day,p))
    return rows
