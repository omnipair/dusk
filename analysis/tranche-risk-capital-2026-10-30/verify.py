"""Independent reconciliation from exported CSV; does not import model.py."""
import csv
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def load_csv(path):
    with path.open() as f:
        return [{k: float(v) for k, v in r.items()} for r in csv.DictReader(f)]


def verify():
    params = json.loads((ROOT / "config.json").read_text())
    maximum_error = 0.0
    checks = 0

    def equal(a, b):
        nonlocal maximum_error, checks
        error = abs(a - b)
        maximum_error = max(error, maximum_error)
        checks += 1
        assert error < 1e-5, (a, b, error)

    paths = sorted((ROOT / "results/traces").glob("*.csv"))
    assert paths, "No exported traces"
    for path in paths:
        rows = load_csv(path)
        for r in rows:
            equal(r["active"], r["senior"] + r["junior"])
            equal(r["active"], r["cash"] + r["debt"])
            equal(r["active"] + r["reserve"] + r["withdrawn"],
                  params["initial_capital"] - r["inventory_loss"] - r["credit_loss"])
            equal(r["junior_income"] + r["senior_income"], r["interest_paid"] + r["swap_income"])
            equal(r["credit_loss"], r["junior_credit_loss"] + r["senior_credit_loss"] + r["insurance_drawn"])
            assert min(r[k] for k in ("cash", "debt", "senior", "junior", "reserve")) >= -1e-5
        for left, right in zip(rows, rows[1:]):
            delta = {k: right[k] - left[k] for k in right}
            equal(delta["interest_paid"], left["debt"] * right["apr"] * delta["day"] / 365)
            equal(delta["swap_income"], left["active"] * params["swap_income_apr"] * delta["day"] / 365)
            equal(delta["debt"], delta["originated_principal"] - delta["repaid_principal"] - delta["liquidated_principal"])
            equal(delta["cash"], delta["repaid_principal"] - delta["originated_principal"] +
                  delta["recovered_principal"] + delta["insurance_drawn"] - delta["inventory_loss"] - delta["withdrawn"])
    result = {"trace_files": len(paths), "identities_checked": checks,
              "maximum_dollar_residual": maximum_error,
              "status": "pass", "scope": "All base-scenario exported balances and interval flows; no empirical calibration."}
    (ROOT / "results/verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    verify()
