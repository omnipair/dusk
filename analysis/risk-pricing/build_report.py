#!/usr/bin/env python3
"""Build the portable HTML report artifact for the Dusk risk-pricing study."""

from __future__ import annotations

import csv
import json
from pathlib import Path


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"


def coerce(value: str) -> object:
    if value == "":
        return None
    try:
        if value.lower() in {"true", "false"}:
            return value.lower() == "true"
    except AttributeError:
        return value
    try:
        return int(value)
    except ValueError:
        pass
    try:
        return float(value)
    except ValueError:
        return value


def read_csv(path: Path) -> list[dict[str, object]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return [{key: coerce(value) for key, value in row.items()} for row in csv.DictReader(handle)]


def query(path: str, sql: str, description: str, tables: list[str]) -> dict[str, object]:
    return {
        "label": description,
        "path": path,
        "query": {
            "language": "sql",
            "engine": "artifact snapshot",
            "sql": sql,
            "description": description,
            "tables_used": tables,
        },
    }


def main() -> None:
    summary = json.loads((RESULTS / "model_summary.json").read_text(encoding="utf-8"))
    scenarios = read_csv(RESULTS / "scenario_results.csv")
    positions = read_csv(RESULTS / "position_pricing.csv")
    structures = read_csv(RESULTS / "pricing_structures.csv")
    strata = read_csv(RESULTS / "stratum_profile.csv")
    central_id = "illustrative_base_075pct_depth"
    central = next(row for row in scenarios if row["scenario_id"] == central_id)
    central_positions = [row for row in positions if row["scenario_id"] == central_id]
    central_structures = [row for row in structures if row["scenario_id"] == central_id]
    base_depth_fees = [row for row in scenarios if row["regime_mix"] == "illustrative_base"]

    for row in scenarios:
        row["regime_label"] = {
            "mostly_calm": "Mostly calm",
            "illustrative_base": "Illustrative base",
            "stress_heavy": "Stress-heavy",
        }[str(row["regime_mix"])]
    for row in central_positions:
        row["liquidation_probability_pct"] = 100.0 * float(row["liquidation_probability"])
        row["position_risk_apr_pct"] = float(row["position_risk_apr_bps"]) / 100.0
        row["pair_risk_apr_pct"] = float(row["pair_risk_apr_bps"]) / 100.0
        row["pair_minus_position_pct_points"] = float(row["cross_subsidy_bps"]) / 100.0

    precedents = [
        {
            "source": "Aave V4",
            "pricing_mechanism": "Base utilization rate multiplied by a borrower collateral-risk factor",
            "calibration_status": "Governance-configured; Ethereum activation set collateral-risk scores to zero",
            "dusk_takeaway": "Separates liquidity price from user risk, but is not actuarial expected-loss pricing",
            "primary_source": "https://aave.com/blog/aave-v4-risk-premiums",
        },
        {
            "source": "Gearbox",
            "pricing_mechanism": "Additive quota rate weighted by collateral exposure, plus quota-increase fee",
            "calibration_status": "Gauge- or curator-configured tariff",
            "dusk_takeaway": "Closest deployed accounting shape for an additive Dusk premium",
            "primary_source": "https://docs.gearbox.finance/core/collateral-limits-specific-rates",
        },
        {
            "source": "IMF DeFi credit-risk annex",
            "pricing_mechanism": "Liquidation probability multiplied by conditional liquidation shortfall",
            "calibration_status": "Research model; includes relative volatility, correlation, bonus, and delay",
            "dusk_takeaway": "Use expected lender loss, not liquidation probability, as the rate numerator",
            "primary_source": "https://www.imf.org/-/media/Files/Publications/GFSR/2022/April/English/ch3onlineannex.ashx",
        },
        {
            "source": "Szpruch et al.",
            "pricing_mechanism": "Overcollateralized loan valued as a perpetual barrier-like option",
            "calibration_status": "Preprint valuation framework",
            "dusk_takeaway": "Borrower optionality and LTV matter; it does not prescribe an origination fee",
            "primary_source": "https://arxiv.org/abs/2409.04233",
        },
        {
            "source": "Halpern, Pass, Saraf",
            "pricing_mechanism": "Shows no single fair continuous rate in its freely repayable perpetual-loan model",
            "calibration_status": "Preprint impossibility result; studies a fee paid on repayment",
            "dusk_takeaway": "A Dusk action fee is a design inference, not this paper's result",
            "primary_source": "https://arxiv.org/abs/2410.11053",
        },
    ]
    (RESULTS / "precedents.json").write_text(json.dumps(precedents, indent=2) + "\n", encoding="utf-8")

    generated_at = summary["generated_at"]
    pair_apr_pct = float(central["pair_risk_apr_bps"]) / 100.0
    high = next(row for row in central_positions if row["position"] == "High LTV")
    mid = next(row for row in central_positions if row["position"] == "Mid LTV")
    low = next(row for row in central_positions if row["position"] == "Low LTV")
    min_apr = min(float(row["pair_risk_apr_bps"]) for row in scenarios) / 100.0
    max_apr = max(float(row["pair_risk_apr_bps"]) for row in scenarios) / 100.0
    el_component_pct = (
        float(central["expected_loss_usdc"])
        / float(central["ead_usdc"])
        * 365.0
        / float(central["horizon_days"])
        * 100.0
    )
    capital_component_pct = pair_apr_pct - el_component_pct

    sources = [
        {"id": "summary", "label": "Model summary and protocol constants", "path": "results/model_summary.json"},
        {"id": "scenarios", "label": "Nine regime and depth scenarios", "path": "results/scenario_results.csv"},
        {"id": "positions", "label": "Shapley position pricing", "path": "results/position_pricing.csv"},
        {"id": "structures", "label": "Pricing-structure comparison", "path": "results/pricing_structures.csv"},
        {"id": "history-profile", "label": "Sealed history stratum profile", "path": "results/stratum_profile.csv"},
        {"id": "validation", "label": "Reproducibility and economic validation", "path": "results/validation.json"},
        {"id": "precedents", "label": "Primary-source precedent ledger", "path": "results/precedents.json"},
        {"id": "notebook", "label": "Executed reproducible notebook", "path": "dusk_risk_pricing.ipynb"},
        {"id": "model-code", "label": "Dependency-free model implementation", "path": "risk_pricing.py"},
        {"id": "aave", "label": "Aave V4 risk premiums", "href": "https://aave.com/blog/aave-v4-risk-premiums"},
        {"id": "aave-launch", "label": "Aave V4 Ethereum activation parameters", "href": "https://governance.aave.com/t/arfc-aave-v4-activation-on-ethereum-mainnet/24293/4"},
        {"id": "gearbox", "label": "Gearbox collateral-specific rates", "href": "https://docs.gearbox.finance/core/collateral-limits-specific-rates"},
        {"id": "imf", "label": "IMF DeFi credit-risk model annex", "href": "https://www.imf.org/-/media/Files/Publications/GFSR/2022/April/English/ch3onlineannex.ashx"},
        {"id": "option-paper", "label": "Pricing and Hedging of Decentralised Lending Contracts", "href": "https://arxiv.org/abs/2409.04233"},
        {"id": "fair-rate-paper", "label": "Fair Interest Rates Are Impossible", "href": "https://arxiv.org/abs/2410.11053"},
        {"id": "binance", "label": "Official Binance public market-data archive", "href": "https://data.binance.vision/?prefix=data/spot/monthly/klines/SOLUSDT/1m/"},
    ]

    charts = [
        {
            "id": "risk-grid",
            "title": "Pair risk APR is dominated by regime mix and liquidation depth",
            "description": "Risk-only add-on; utilization APR is excluded. Each bar prices gross delayed-liquidation shortfall plus a 15% annual charge on ES99 economic capital.",
            "showDescription": True,
            "dataset": "scenarios",
            "type": "bar",
            "source": query(
                "results/scenario_results.csv",
                "SELECT regime_label, depth_label, pair_risk_apr_bps FROM scenarios ORDER BY regime_label, depth_factor DESC",
                "Nine explicit regime-mix and liquidation-depth scenarios",
                ["scenarios"],
            ),
            "encodings": {
                "x": {"field": "depth_label", "type": "nominal", "title": "Available liquidation depth"},
                "y": {"field": "pair_risk_apr_bps", "type": "quantitative", "title": "Risk APR (bps)"},
                "color": {"field": "regime_label", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "position-premiums",
            "title": "One pair rate hides a 0% to 58.7% position-risk range",
            "description": "Central illustrative delayed-liquidation stress: 55% calm, 35% typical, 10% stress per 12-hour segment, with 75% of starting depth.",
            "showDescription": True,
            "dataset": "central_positions",
            "type": "bar",
            "source": query(
                "results/position_pricing.csv",
                f"SELECT position, impact_ltv_pct, position_risk_apr_bps, pair_risk_apr_bps FROM central_positions WHERE scenario_id = '{central_id}' ORDER BY impact_ltv_pct",
                "Shapley allocation of annual risk cost in the central illustrative scenario",
                ["central_positions"],
            ),
            "encodings": {
                "x": {"field": "position", "type": "nominal", "title": "Borrower position"},
                "y": {"field": "position_risk_apr_bps", "type": "quantitative", "title": "Allocated risk APR (bps)"},
                "color": {"field": "position", "type": "nominal"},
            },
        },
        {
            "id": "marginal-fee-depth",
            "title": "The high-LTV action fee rises nonlinearly as depth disappears",
            "description": "Upfront seven-day risk cost of adding the 80% impact-LTV position to the low- and mid-LTV book under the illustrative regime mix.",
            "showDescription": True,
            "dataset": "base_depth_fees",
            "type": "bar",
            "source": query(
                "results/scenario_results.csv",
                "SELECT depth_label, marginal_high_ltv_fee_bps FROM base_depth_fees ORDER BY depth_factor DESC",
                "Marginal portfolio risk cost for the high-LTV position",
                ["base_depth_fees"],
            ),
            "encodings": {
                "x": {"field": "depth_label", "type": "nominal", "title": "Available liquidation depth"},
                "y": {"field": "marginal_high_ltv_fee_bps", "type": "quantitative", "title": "Upfront fee (bps)"},
            },
        },
    ]

    tables = [
        {
            "id": "central-position-table",
            "title": "Central position pricing and pair-rate cross-subsidy",
            "description": "Positive pair-minus-position values mean the common pair rate overcharges that position; negative values mean it undercharges it.",
            "showDescription": True,
            "dataset": "central_positions",
            "source": query(
                "results/position_pricing.csv",
                f"SELECT position, impact_ltv_pct, debt_usdc, liquidation_probability_pct, position_risk_apr_pct, pair_minus_position_pct_points FROM central_positions WHERE scenario_id = '{central_id}' ORDER BY impact_ltv_pct",
                "Central Shapley position allocation and common-rate cross-subsidy",
                ["central_positions"],
            ),
            "columns": [
                {"field": "position", "label": "Position"},
                {"field": "impact_ltv_pct", "label": "Impact LTV (%)", "format": "number"},
                {"field": "debt_usdc", "label": "Debt (USDC)", "format": "number"},
                {"field": "liquidation_probability_pct", "label": "Liquidation probability (%)", "format": "number"},
                {"field": "position_risk_apr_pct", "label": "Position risk APR (%)", "format": "number"},
                {"field": "pair_minus_position_pct_points", "label": "Pair minus position (pp)", "format": "number"},
            ],
        },
        {
            "id": "structure-table",
            "title": "Three ways Dusk could collect the same risk cost",
            "description": "Quotes are central-scenario illustrations; each structure allocates the charge differently.",
            "showDescription": True,
            "dataset": "central_structures",
            "source": query(
                "results/pricing_structures.csv",
                f"SELECT pricing_structure, quote, prices, accounting_fit FROM central_structures WHERE scenario_id = '{central_id}'",
                "Pricing structure comparison for the central illustrative scenario",
                ["central_structures"],
            ),
            "columns": [
                {"field": "pricing_structure", "label": "Structure"},
                {"field": "quote", "label": "Illustrative quote"},
                {"field": "prices", "label": "What it prices"},
                {"field": "accounting_fit", "label": "Dusk accounting fit"},
            ],
        },
        {
            "id": "precedent-table",
            "title": "Deployed precedents stop short of actuarial expected-loss pricing",
            "description": "Production protocols have risk tariffs; the Dusk model supplies an expected-loss calibration layer.",
            "showDescription": True,
            "dataset": "precedents",
            "source": query(
                "results/precedents.json",
                "SELECT source, pricing_mechanism, calibration_status, dusk_takeaway, primary_source FROM precedents",
                "Primary project documentation and research papers",
                ["precedents"],
            ),
            "columns": [
                {"field": "source", "label": "Source"},
                {"field": "pricing_mechanism", "label": "Mechanism"},
                {"field": "calibration_status", "label": "Status"},
                {"field": "dusk_takeaway", "label": "Dusk takeaway"},
            ],
        },
        {
            "id": "history-table",
            "title": "The historical input is deliberately stratified",
            "description": "Five sealed 12-hour score windows per stratum; these do not reveal unconditional real-world regime frequencies.",
            "showDescription": True,
            "dataset": "strata",
            "source": query(
                "results/stratum_profile.csv",
                "SELECT stratum, windows, median_12h_return_pct, median_12h_minimum_from_start_pct, worst_12h_minimum_from_start_pct FROM strata ORDER BY stratum",
                "Profile of the sealed SOLUSDT score windows",
                ["strata"],
            ),
            "columns": [
                {"field": "stratum", "label": "Stratum"},
                {"field": "windows", "label": "Windows", "format": "number"},
                {"field": "median_12h_return_pct", "label": "Median 12h return (%)", "format": "number"},
                {"field": "median_12h_minimum_from_start_pct", "label": "Median minimum (%)", "format": "number"},
                {"field": "worst_12h_minimum_from_start_pct", "label": "Worst minimum (%)", "format": "number"},
            ],
        },
    ]

    blocks = [
        {"id": "title", "type": "markdown", "body": "# Dusk risk pricing: a SOL/USDC worked example"},
        {
            "id": "executive-summary",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## Executive summary\n\n"
                f"The model's central **delayed-liquidation stress** produces a **{pair_apr_pct:.2f}% risk-only pair APR**, on top of—not inside—the utilization IRM. That common rate masks a **{float(low['position_risk_apr_pct']):.2f}% / {float(mid['position_risk_apr_pct']):.2f}% / {float(high['position_risk_apr_pct']):.2f}%** Shapley allocation across 50%, 65%, and 80% impact-LTV positions. The pair rate therefore overcharges the low-LTV position by **{float(low['pair_minus_position_pct_points']):.2f} percentage points** and undercharges the high-LTV position by **{abs(float(high['pair_minus_position_pct_points'])):.2f} points**.\n\n"
                f"For the same state, the marginal seven-day fee for adding the 80% LTV position is **{float(central['marginal_high_ltv_fee_bps']):.1f} bps upfront**. Across the declared regime/depth grid, the pair quote ranges from **{min_apr:.2f}% to {max_apr:.2f}% APR**. These are scenario prices, not production parameters."
            ),
        },
        {
            "id": "definitions",
            "type": "markdown",
            "sourceId": "model-code",
            "body": (
                "## What is being priced\n\n"
                "This is lender-loss pricing, not a safeguard score. For horizon `H`, the model uses:\n\n"
                "`risk cost = expected gross shortfall + 15% × H × (ES99 − expected shortfall)`\n\n"
                "`risk APR = expected shortfall / (debt × H) + 15% × (ES99 − expected shortfall) / debt`\n\n"
                "Liquidation probability alone is not charged: most prompt liquidations repay lenders. Loss appears only when post-trigger price/depth deterioration makes collateral recovery insufficient. The modeled shortfall is gross of any pre-funded USDC insurance draw."
            ),
        },
        {
            "id": "setup",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## Representative Dusk market and assumptions\n\n"
                "The worked book is SOL collateral against USDC debt in a balanced **$2.0M default-CPMM market** at $150/SOL. Three positions each pledge 333.33 SOL at 50%, 65%, and 80% impact LTV; total debt is about $92.8k. Dusk inputs are the 85% liquidation collateral factor, 80.75% maximum borrow factor, 1%–5% incentive, 2% insurance-funding penalty, 500 ppm price safety, and 25 ppm Q haircut.\n\n"
                "Fifty thousand seven-day paths bootstrap 14 sealed 12-hour SOLUSDT score segments. The central scenario draws each segment from a 55% calm / 35% typical / 10% stress mixture and assumes 75% of starting liquidation depth. USDT is only a return proxy for USDC."
            ),
        },
        {
            "id": "timing-warning",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "### Read the 24.44% correctly\n\n"
                "Recovery is valued at the **seven-day path minimum after trigger**. That is a deliberately conservative delayed-liquidation stress, while Dusk's auction floor becomes reachable in roughly 21 seconds. At the trigger boundary itself, collateral still covers debt plus the configured penalty. The large quote therefore measures gap/depth risk under delayed recovery; it must not be installed as a live APR without keeper-fill and execution-delay calibration."
            ),
        },
        {"id": "risk-grid-chart", "type": "chart", "chartId": "risk-grid"},
        {
            "id": "risk-grid-explanation",
            "type": "markdown",
            "sourceId": "scenarios",
            "body": (
                "The scenario fan is the useful output—not the central point estimate. At unchanged utilization, moving from 100% to 50% depth roughly doubles or quadruples the price depending on the regime mix. In the central case, the **{:.2f}%** APR decomposes into **{:.2f}%** annualized expected shortfall and **{:.2f}%** tail-capital return.".format(pair_apr_pct, el_component_pct, capital_component_pct)
            ),
        },
        {
            "id": "allocation-text",
            "type": "markdown",
            "sourceId": "positions",
            "body": (
                "## Pair pricing is operationally easy but economically blunt\n\n"
                "Dusk currently has one shared borrow index per asset side, so a pair-wide spread is the smallest accounting change. Economically, it invites adverse selection: the 80% LTV borrower is underpriced by more than 34 percentage points in the central stress while the 50% LTV borrower pays for risk it does not add in these paths."
            ),
        },
        {"id": "position-chart", "type": "chart", "chartId": "position-premiums"},
        {
            "id": "position-chart-explanation",
            "type": "markdown",
            "sourceId": "positions",
            "body": "The Shapley calculation reruns every borrower coalition, so each position receives both its stand-alone loss and its share of the nonlinear shared-depth externality. Allocations reconcile exactly to the market risk cost. A zero estimate for the 50% LTV position means zero loss in the declared paths—not zero real-world risk.",
        },
        {"id": "central-position-table-block", "type": "table", "tableId": "central-position-table"},
        {
            "id": "collection-text",
            "type": "markdown",
            "sourceId": "structures",
            "body": (
                "## Collection choice: rolling premium versus marginal action fee\n\n"
                "The economically clean target is a position-level additive premium: `borrow APR = utilization APR + position risk APR`. It needs separate premium debt or a second index because today's quote-debt index is shared. A practical interim route is a marginal fee whenever a borrower increases debt or withdraws collateral; the fee is recomputed from the whole book, so concentration and shared liquidation depth are included. Calling it an origination result from the option papers would be incorrect—it is a Dusk design inference."
            ),
        },
        {"id": "marginal-chart", "type": "chart", "chartId": "marginal-fee-depth"},
        {
            "id": "marginal-chart-explanation",
            "type": "markdown",
            "sourceId": "scenarios",
            "body": "The action fee is nonlinear because adding the high-LTV position can consume recovery depth already relied upon by existing debt. It should be recomputed on every risk-increasing action and tied to an explicit seven-day repricing horizon.",
        },
        {"id": "structure-table-block", "type": "table", "tableId": "structure-table"},
        {
            "id": "precedent-text",
            "type": "markdown",
            "sourceId": "precedents",
            "body": (
                "## What exists elsewhere\n\n"
                "[Aave V4](https://aave.com/blog/aave-v4-risk-premiums) separates utilization from borrower collateral risk, but its premium is a configured multiplicative factor; Ethereum activation set the scores to zero. [Gearbox](https://docs.gearbox.finance/core/collateral-limits-specific-rates) uses the closest deployed additive shape, with collateral quota rates and a quota-increase fee, but those are configured tariffs. The [IMF annex](https://www.imf.org/-/media/Files/Publications/GFSR/2022/April/English/ch3onlineannex.ashx) supplies the direct expected-loss decomposition used here."
            ),
        },
        {"id": "precedent-table-block", "type": "table", "tableId": "precedent-table"},
        {
            "id": "recommendation",
            "type": "markdown",
            "sourceId": "validation",
            "body": (
                "## Decision\n\n"
                "Run a **shadow position quote**, not a production rate change, with three outputs on each risk-increasing action: seven-day marginal expected loss, ES99 capital charge, and the equivalent additive APR. Use position-level premium accounting as the target architecture; use the marginal action fee only as an accounting-compatible bridge. Do not ship a pair-average premium except as a temporary portfolio surcharge because its cross-subsidy is structurally large."
            ),
        },
        {
            "id": "calibration-plan",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## What would make the quote production-calibrated\n\n"
                "1. Record trigger-to-bid and trigger-to-floor timing, bid premiums, failed auctions, and realized collateral resale values.\n"
                "2. Replay minute/slot paths to the first trigger and simulate repeated 50% closes rather than valuing recovery at the horizon minimum.\n"
                "3. Include external venue depth, keeper profit/gas, funded USDC insurance, repayment duration, isolated leverage, and hLP debt.\n"
                "4. Backtest whether quoted premium revenue covers realized shortfall by LTV bucket and re-estimate the regime transition model."
            ),
        },
        {"id": "history-table-block", "type": "table", "tableId": "history-table"},
        {
            "id": "limitations",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## Caveats and further questions\n\n"
                "The history contains five deliberately selected calm, typical, and stress windows; the regime weights are assumptions. SOLUSDT proxies SOL/USDC. The joint CPMM unwind represents shared resale capacity even though Dusk auctions transfer collateral to external liquidators. No funded-insurance balance, borrower repayment behavior, oracle/external-market basis, jump overlay, or keeper failure model is calibrated. The output is therefore a reproducible stress-pricing laboratory, not an actuarial live quote."
            ),
        },
    ]

    artifact = {
        "surface": "report",
        "manifest": {
            "version": 1,
            "surface": "report",
            "title": "Dusk risk pricing: a SOL/USDC worked example",
            "description": "A reproducible expected-shortfall and tail-capital comparison of pair-average, marginal-fee, and position-level risk pricing for Dusk.",
            "generatedAt": generated_at,
            "sources": sources,
            "charts": charts,
            "tables": tables,
            "blocks": blocks,
        },
        "snapshot": {
            "version": 1,
            "status": "ready",
            "generatedAt": generated_at,
            "datasets": {
                "scenarios": scenarios,
                "central_positions": central_positions,
                "base_depth_fees": base_depth_fees,
                "central_structures": central_structures,
                "strata": strata,
                "precedents": precedents,
            },
        },
        "sources": sources,
    }
    output = HERE / "artifact.json"
    output.write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
