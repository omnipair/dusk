"""One-shot quote comparison against the existing frozen Dusk native harness."""

import hashlib
import json
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
OLD = HERE.parent / "oracle-guided-amm-2026-10-16"
BIN = OLD / "native/target/debug/dusk-oracle-guided-replay"


def call(process, sequence, payload):
    process.stdin.write(json.dumps({"id": sequence, **payload}) + "\n")
    process.stdin.flush()
    answer = json.loads(process.stdout.readline())
    if not answer.get("ok") or answer.get("id") != sequence:
        raise RuntimeError(answer)
    return answer["data"]


if __name__ == "__main__":
    if not BIN.exists():
        raise SystemExit("Build the frozen harness using its native/README.md first")
    process = subprocess.Popen([str(BIN)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        initial = call(process, 1, {
            "op": "init", "state": "native", "price": 100.0,
            "capital_quote": 1_000_000, "hlp_each_quote": 0,
            "base_decimals": 6, "quote_decimals": 6,
            "amplification": 5, "base_fee_bps": 3,
            "dynamic_cap_bps": 20,
            "clock": {"timestamp": 1750291260, "slot": 2500},
        })
        buy = call(process, 2, {
            "op": "quote", "state": "native", "asset_in": "quote",
            "amount_in": 10_000_000_000, "compact": True,
        })["result"]
        sell = call(process, 3, {
            "op": "quote", "state": "native", "asset_in": "base",
            "amount_in": 100_000_000, "compact": True,
        })["result"]
    finally:
        process.stdin.close()
        process.wait(timeout=20)
    buy_base = buy["amount_out"] / 1_000_000
    sell_quote = sell["amount_out"] / 1_000_000
    result = {
        "binary_sha256": hashlib.sha256(BIN.read_bytes()).hexdigest(),
        "source_commit_claim": json.loads((OLD / "config.json").read_text())["source_commit"],
        "configuration": "pure yLP, $1m balanced, 5x center, 3bp base fee, native dynamic fees",
        "seeded_price": initial["seeded_launch_price"],
        "buy_10k_base_out": buy_base,
        "buy_10k_average_price": 10_000 / buy_base,
        "sell_100_base_quote_out": sell_quote,
        "sell_100_base_average_price": sell_quote / 100,
        "buy_fee_rate_nad": buy["fee_rate_nad"],
        "sell_fee_rate_nad": sell["fee_rate_nad"],
    }
    (HERE / "native_static.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
