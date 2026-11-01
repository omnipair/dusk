use dusk_c1_q48_reference::{BridgeStart, FiniteC1, SolverIterations, NAD, Q48};

#[derive(Clone, Copy, Debug, Default)]
struct Summary {
    quotes: u32,
    q_evaluations: u64,
    inner_q_iterations: u64,
    invariant_iterations: u64,
    reserve_iterations: u64,
    exact_tail_quotes: u32,
}

impl Summary {
    fn record(&mut self, iterations: SolverIterations, used_exact_tail: bool) {
        self.quotes += 1;
        self.q_evaluations += u64::from(iterations.q_evaluations);
        self.inner_q_iterations += u64::from(iterations.inner_q_iterations);
        self.invariant_iterations += u64::from(iterations.invariant_iterations);
        self.reserve_iterations += u64::from(iterations.reserve_iterations);
        self.exact_tail_quotes += u32::from(used_exact_tail);
    }

    const fn total_iterations(self) -> u64 {
        self.inner_q_iterations + self.invariant_iterations + self.reserve_iterations
    }
}

fn state_for_ratio(ratio_q48: u128, high_first: bool) -> (u64, u64) {
    let low = 1_000_000_000_000_u64;
    let product = (low as u128) * ratio_q48;
    let high = u64::try_from(product.div_ceil(Q48)).unwrap();
    if high_first {
        (high, low)
    } else {
        (low, high)
    }
}

fn summarize(bridge: BridgeStart) -> Summary {
    let mut summary = Summary::default();
    for (peak, scale) in [
        (20 * NAD, 50_000_000),
        (200 * NAD, 5_000_000),
        (2_000 * NAD, 500_000),
    ] {
        let model = FiniteC1::new(peak, scale, bridge).unwrap();
        let geometry = model.geometry();
        let ratios = [
            Q48,
            (Q48 + geometry.reserve_ratio_start_q48) / 2,
            (geometry.reserve_ratio_start_q48 + geometry.reserve_ratio_tail_q48) / 2,
            geometry.reserve_ratio_tail_q48 + (geometry.reserve_ratio_tail_q48 - Q48),
        ];
        for ratio in ratios {
            for high_first in [true, false] {
                let (x, y) = state_for_ratio(ratio, high_first);
                for divisor in [10_000_u64, 1_000, 100] {
                    let exact_in = model.quote_exact_in(x, y, (x / divisor).max(1)).unwrap();
                    summary.record(exact_in.iterations, exact_in.used_exact_tail);
                    let exact_out = model.quote_exact_out(x, y, (y / divisor).max(1)).unwrap();
                    summary.record(exact_out.iterations, exact_out.used_exact_tail);
                }
            }
        }
    }
    summary
}

fn main() {
    println!(
        "bridge,quotes,q_evaluations,inner_q_iterations,invariant_iterations,reserve_iterations,total_iterations,exact_tail_quotes"
    );
    for (name, bridge) in [
        ("quarter", BridgeStart::Quarter),
        ("half", BridgeStart::Half),
    ] {
        let summary = summarize(bridge);
        println!(
            "{name},{},{},{},{},{},{},{}",
            summary.quotes,
            summary.q_evaluations,
            summary.inner_q_iterations,
            summary.invariant_iterations,
            summary.reserve_iterations,
            summary.total_iterations(),
            summary.exact_tail_quotes,
        );
    }
}
