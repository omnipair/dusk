"""Synthetic, cash-conserving economic experiment. See PREANALYSIS.md."""
from __future__ import annotations

from dataclasses import dataclass, field
from math import exp, log

EPS = 1e-8
POLICIES = ("reserve_capped", "reserve_uncapped", "junior_fixed", "junior_share", "junior_debt")


@dataclass(frozen=True)
class Shock:
    day: float
    price: float
    execution_discount: float
    affected_debt_fraction: float
    liquid_inventory_drawdown: float


@dataclass(frozen=True)
class Scenario:
    name: str
    shocks: tuple[Shock, ...] = ()
    growth_day: float = 999
    demand_multiplier: float = 1.0
    exit_day: float = 999
    external_exit_fraction: float = 0.0
    warning_start: float = 999
    warning_end: float = 999
    hurdle_addition: float = 0.0


SCENARIOS = (
    Scenario("calm"),
    Scenario("demand_growth", growth_day=20, demand_multiplier=1.5),
    Scenario("moderate_crash", (Shock(40, .60, .10, .50, .15),)),
    Scenario("joint_severe", (Shock(40, .45, .20, .75, .35),)),
    Scenario("run_then_crash", (Shock(40, .60, .10, .50, .25),),
             exit_day=35, external_exit_fraction=.5,
             warning_start=30, warning_end=55, hurdle_addition=.20),
    Scenario("repeated_crash", (Shock(40, .60, .10, .50, .15),
                                Shock(45, .45, .20, .75, .25))),
    Scenario("growth_then_crash", (Shock(40, .45, .20, .75, .35),),
             growth_day=20, demand_multiplier=1.5),
)


@dataclass
class Exit:
    shares: float
    ready_day: float
    cash: bool


@dataclass
class State:
    policy: str
    cash: float
    debt: float
    senior: float
    junior: float
    reserve: float
    junior_shares: float
    apr: float
    anchor: float
    signal_ema: float
    reserve_window_day: int = -1
    reserve_window_remaining: float = 0.0
    exits: list[Exit] = field(default_factory=list)
    totals: dict[str, float] = field(default_factory=lambda: dict.fromkeys((
        "inventory_loss", "credit_loss", "junior_credit_loss", "senior_credit_loss",
        "insurance_drawn", "interest_paid", "swap_income", "junior_income",
        "senior_income", "withdrawn", "exit_requested", "converted_to_junior",
        "converted_to_senior", "liquidated_principal", "recovered_principal",
        "originated_principal", "repaid_principal", "junior_wipeouts"), 0.0))

    @property
    def active(self):
        return self.senior + self.junior

    @property
    def nav(self):
        return self.junior / self.junior_shares if self.junior_shares > EPS else 1.0

    @property
    def pending_shares(self):
        return sum(x.shares for x in self.exits)

    @property
    def pending_value(self):
        return self.pending_shares * self.nav

    @property
    def committed_junior(self):
        return max(0.0, self.junior - self.pending_value)


def initialize(policy, p):
    j = p["initial_capital"] * p["initial_junior_fraction"]
    s = p["initial_capital"] - j
    reserve = j if policy.startswith("reserve") else 0.0
    junior = 0.0 if reserve else j
    signal = p["initial_junior_fraction"] if policy == "junior_share" else j / p["initial_debt"]
    return State(policy, s + junior - p["initial_debt"], p["initial_debt"],
                 s, junior, reserve, junior, p["initial_apr"], p["initial_apr"], signal)


def liquidatable(price, p):
    # Execution depth never appears in the trigger.
    return p["origination_ltv"] / price >= p["liquidation_ltv"]


def recovery_fraction(price, execution_discount, p):
    return min(1.0, price / p["origination_ltv"] * (1 - execution_discount))


def reset_wiped_epoch(st):
    if st.junior <= EPS and st.junior_shares > EPS:
        st.junior = 0.0
        st.junior_shares = 0.0
        st.exits.clear()
        st.totals["junior_wipeouts"] += 1


def apply_shock(st, shock, p):
    before = {k: st.totals[k] for k in ("inventory_loss", "credit_loss", "junior_credit_loss",
                                       "senior_credit_loss", "insurance_drawn")}
    debt_before, junior_before, cash_before, reserve_before = st.debt, st.junior, st.cash, st.reserve
    mark_loss = st.cash * min(1.0, shock.liquid_inventory_drawdown * p["inventory_loss_multiplier"])
    active_before = st.active
    junior_mark = mark_loss * st.junior / active_before if active_before else 0.0
    st.cash -= mark_loss
    st.junior -= junior_mark
    st.senior -= mark_loss - junior_mark
    st.totals["inventory_loss"] += mark_loss
    loss = 0.0
    if liquidatable(shock.price, p):
        principal = st.debt * shock.affected_debt_fraction
        recovery = principal * recovery_fraction(shock.price, shock.execution_discount, p)
        loss = principal - recovery
        st.debt -= principal
        st.cash += recovery
        st.totals["liquidated_principal"] += principal
        st.totals["recovered_principal"] += recovery
        st.totals["credit_loss"] += loss
        if st.policy.startswith("reserve"):
            # A simultaneous batch consists of separate liquidation events.
            if int(shock.day) != st.reserve_window_day:
                st.reserve_window_day = int(shock.day)
                st.reserve_window_remaining = st.reserve * p["insurance_day_fraction"]
            covered = 0.0
            for _ in range(p["liquidations_per_shock"]):
                event_loss = loss / p["liquidations_per_shock"]
                draw = min(event_loss, st.reserve)
                if st.policy == "reserve_capped":
                    draw = min(draw, st.reserve * p["insurance_event_fraction"], st.reserve_window_remaining)
                    st.reserve_window_remaining -= draw
                st.reserve -= draw
                covered += draw
            st.cash += covered
            st.senior -= loss - covered
            st.totals["insurance_drawn"] += covered
            st.totals["senior_credit_loss"] += loss - covered
        else:
            j_loss = min(st.junior, loss)
            st.junior -= j_loss
            st.senior -= loss - j_loss
            st.totals["junior_credit_loss"] += j_loss
            st.totals["senior_credit_loss"] += loss - j_loss
        reset_wiped_epoch(st)
    return {"day": shock.day, "price": shock.price, "execution_discount": shock.execution_discount,
            "debt_before": debt_before, "junior_before": junior_before,
            "cash_before": cash_before, "reserve_before": reserve_before,
            "liquidation_trigger_ltv": p["liquidation_ltv"],
            **{k: st.totals[k] - v for k, v in before.items()}}


def request_exit(st, amount, day, p, cash=False):
    amount = min(max(0.0, amount), st.committed_junior)
    if amount <= EPS or st.nav <= EPS:
        return
    st.exits.append(Exit(amount / st.nav, day + p["exit_notice_days"], cash))
    if cash:
        st.totals["exit_requested"] += amount


def settle_exits(st, day):
    pending = []
    for req in st.exits:
        if req.ready_day > day + EPS:
            pending.append(req)
            continue
        nav = st.nav
        amount = min(st.junior, req.shares * nav)
        paid = min(amount, st.cash) if req.cash else amount
        shares = paid / nav if nav > EPS else req.shares
        st.junior -= paid
        st.junior_shares = max(0.0, st.junior_shares - shares)
        if req.cash:
            st.cash -= paid
            st.totals["withdrawn"] += paid
        else:
            st.senior += paid
            st.totals["converted_to_senior"] += paid
        if req.shares - shares > EPS:
            pending.append(Exit(req.shares - shares, req.ready_day, req.cash))
    st.exits = pending


def enter_junior(st, amount):
    reset_wiped_epoch(st)
    amount = min(max(0.0, amount), st.senior)
    nav = st.nav
    st.senior -= amount
    st.junior += amount
    st.junior_shares += amount / nav
    st.totals["converted_to_junior"] += amount


def allocate(st, day, scenario, p):
    if st.policy.startswith("reserve") or p["lp_response_days"] <= 0:
        return
    a = st.active
    if a <= EPS:
        return
    alpha = p["junior_interest_share"]
    # Finite quote at zero junior uses a disclosed 0.5%-of-equity entry lot.
    j_quote = max(st.junior, .005 * a)
    s_quote = max(st.senior, .005 * a)
    spread = alpha * st.apr * st.debt / j_quote - (1 - alpha) * st.apr * st.debt / s_quote
    j0 = p["initial_capital"] * p["initial_junior_fraction"]
    s0 = p["initial_capital"] - j0
    # Fixed at the BASE 50/50 split, even when testing different revenue shares.
    hurdle = .5 * p["initial_apr"] * p["initial_debt"] * (1 / j0 - 1 / s0)
    if scenario.warning_start <= day < scenario.warning_end:
        hurdle += scenario.hurdle_addition
    q0 = p["initial_junior_fraction"]
    logit = log(q0 / (1 - q0)) + (spread - hurdle) / p["lp_spread_scale"]
    target_q = 1 / (1 + exp(-max(-30.0, min(30.0, logit))))
    step = 1 - exp(-p["step_days"] / p["lp_response_days"])
    target_j = target_q * a
    if target_j > st.junior:
        enter_junior(st, (target_j - st.junior) * step)
    elif target_j < st.committed_junior:
        request_exit(st, (st.committed_junior - target_j) * step, day, p)


def update_rate(st, p):
    dt = p["step_days"]
    if st.policy.startswith("reserve"):
        u = st.debt / st.active if st.active > EPS else 1.0
        t = p["utilization_target"]
        e = (u - t) / (t if u <= t else 1 - t)
        e = max(-1.0, min(1.0, e))
        k = p["utilization_steepness"]
        curve = 1 + (k - 1) * e if e >= 0 else 1 + (1 - 1 / k) * e
        st.apr = min(p["max_apr"], max(p["min_apr"], st.anchor * curve))
        exponent = max(-.5, min(.5, p["utilization_adaptation_per_year"] * e * dt / 365))
        st.anchor = max(.001, min(2.0, st.anchor * (1 + exponent)))
    elif st.policy == "junior_fixed":
        st.apr = p["initial_apr"]
    else:
        if st.policy == "junior_share":
            signal = st.committed_junior / max(st.active, EPS)
            target = p["initial_junior_fraction"]
        else:
            target = p["initial_capital"] * p["initial_junior_fraction"] / p["initial_debt"]
            signal = st.committed_junior / st.debt if st.debt > EPS else 2 * target
        w = 1 - exp(-dt / p["coverage_up_smoothing_days"])
        st.signal_ema += w * (signal - st.signal_ema)
        usable = min(signal, st.signal_ema)  # recognize deterioration immediately
        e = max(-1.0, min(1.0, (target - usable) / target))
        if abs(e) <= p["coverage_band_fraction"]:
            e = 0.0
        else:
            e = ((abs(e) - p["coverage_band_fraction"]) /
                 (1 - p["coverage_band_fraction"])) * (1 if e > 0 else -1)
        st.apr = min(p["max_apr"], max(p["min_apr"],
                     st.apr * exp(log(2) * e * dt / p["controller_half_life_days"])))


def income_and_borrowing(st, day, scenario, p):
    dt = p["step_days"]
    interest = st.debt * st.apr * dt / 365
    fees = st.active * p["swap_income_apr"] * dt / 365
    j_share = st.junior / st.active if st.active > EPS else 0.0
    # A zero junior epoch earns nothing. Revenue is fully conserved.
    j_interest = interest * p["junior_interest_share"] if st.junior > EPS else 0.0
    j_income = j_interest + fees * j_share
    st.totals["interest_paid"] += interest
    st.totals["swap_income"] += fees
    st.totals["junior_income"] += j_income
    st.totals["senior_income"] += interest + fees - j_income
    multiplier = scenario.demand_multiplier if day >= scenario.growth_day else 1.0
    desired = min(2 * p["initial_debt"], p["initial_debt"] * multiplier *
                  (st.apr / p["initial_apr"]) ** (-p["borrower_elasticity"]))
    move = (desired - st.debt) * (1 - exp(-dt / p["borrower_response_days"]))
    if move > 0:
        move = min(move, st.cash)
        st.totals["originated_principal"] += move
    else:
        move = max(move, -st.debt)
        st.totals["repaid_principal"] -= move
    st.debt += move
    st.cash -= move


def check_state(st, p):
    if min(st.cash, st.debt, st.senior, st.junior, st.reserve) < -1e-5:
        raise AssertionError("Negative balance")
    assert abs(st.cash + st.debt - st.active) < 1e-5
    expected = p["initial_capital"] - st.totals["inventory_loss"] - st.totals["credit_loss"]
    assert abs(st.active + st.reserve + st.totals["withdrawn"] - expected) < 1e-5
    assert st.pending_shares <= st.junior_shares + 1e-5
    assert abs(st.totals["junior_income"] + st.totals["senior_income"] -
               st.totals["interest_paid"] - st.totals["swap_income"]) < 1e-5
    assert abs(st.totals["credit_loss"] - st.totals["junior_credit_loss"] -
               st.totals["senior_credit_loss"] - st.totals["insurance_drawn"]) < 1e-5


def snapshot(st, day):
    return {"day": day, "cash": st.cash, "debt": st.debt, "senior": st.senior,
            "junior": st.junior, "reserve": st.reserve, "active": st.active,
            "apr": st.apr, "pending_exit_value": st.pending_value,
            "committed_junior": st.committed_junior, **st.totals}


def simulate(policy, scenario, p):
    st = initialize(policy, p)
    rows, events = [snapshot(st, 0.0)], []
    integrals = dict.fromkeys(("debt", "active", "cash", "reserve", "junior", "apr"), 0.0)
    peak_apr = st.apr
    # Boundary events happen after prior-period accrual, before exits at that boundary.
    # A shock and an exit at exactly the same time give the loss priority.
    for i in range(round(p["days"] / p["step_days"])):
        day, end = i * p["step_days"], (i + 1) * p["step_days"]
        update_rate(st, p)
        for key in integrals:
            integrals[key] += getattr(st, key) * p["step_days"]
        peak_apr = max(peak_apr, st.apr)
        income_and_borrowing(st, day, scenario, p)
        for shock in scenario.shocks:
            if abs(end - shock.day) < EPS:
                events.append(apply_shock(st, shock, p))
        if abs(end - scenario.exit_day) < EPS and not policy.startswith("reserve"):
            request_exit(st, scenario.external_exit_fraction * st.committed_junior, end, p, cash=True)
        settle_exits(st, end)
        allocate(st, end, scenario, p)
        # Zero-notice allocation switches settle at the request boundary too.
        settle_exits(st, end)
        check_state(st, p)
        rows.append(snapshot(st, end))
    total_income = st.totals["interest_paid"] + st.totals["swap_income"]
    summary = {"policy": policy, "scenario": scenario.name,
               **{"average_" + k: v / p["days"] for k, v in integrals.items()},
               "debt_per_initial_capital": integrals["debt"] / p["days"] / p["initial_capital"],
               "active_per_initial_capital": integrals["active"] / p["days"] / p["initial_capital"],
               "borrower_paid_apr": st.totals["interest_paid"] * 365 / max(integrals["debt"], EPS),
               "peak_apr": peak_apr, "end_debt": st.debt, "end_junior": st.junior,
               "end_reserve": st.reserve, "end_pending_exit_value": st.pending_value,
               "max_pending_exit_value": max(r["pending_exit_value"] for r in rows),
               "total_capital_pnl": total_income - st.totals["inventory_loss"] - st.totals["credit_loss"],
               **st.totals}
    return summary, rows, events
