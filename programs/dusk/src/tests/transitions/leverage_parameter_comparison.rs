// Sensitivity experiments only. None of these values change program policy.
#[derive(Clone, Copy)]
pub(super) struct Policy {
    pub wallet_bps: u16,
    pub crowding_denominator: u16,
    pub recovery_buffer_bps: u16,
    pub critical_fraction_bps: u64,
    pub max_reward_bps: u64,
    pub insurance_bps: u64,
    pub keeper_cost: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            wallet_bps: 5_000,
            crowding_denominator: 10,
            recovery_buffer_bps: 200,
            critical_fraction_bps: 5_000,
            max_reward_bps: 100,
            insurance_bps: 20,
            keeper_cost: 50,
        }
    }
}

#[test]
fn native_parameter_comparison_report() {
    let full = std::env::var_os("DUSK_PARAMETER_SWEEP").is_some();
    println!("NATIVE_PARAMETERS,variant,amplification,controller,hlp,pieces,path,wallet_bps,opened,original_debt,min_entry_health_bps,max_entry_requirement_bps,partial,flash,solvent,emergency,repaid,insurance,loss,owner,reward,contribution,remaining,eligible,eligible_debt,below_keeper_cost,failed_fills,blocked");
    let mut count = 0;
    for (name, policy) in [
        ("baseline", Policy::default()),
        (
            "critical_75",
            Policy {
                critical_fraction_bps: 7_500,
                ..Policy::default()
            },
        ),
        (
            "reward_025",
            Policy {
                max_reward_bps: 25,
                ..Policy::default()
            },
        ),
        (
            "recovery_1",
            Policy {
                recovery_buffer_bps: 100,
                ..Policy::default()
            },
        ),
        (
            "crowding_steeper",
            Policy {
                crowding_denominator: 5,
                ..Policy::default()
            },
        ),
    ] {
        for amp in [1, 4, 10] {
            for controller in [false, true] {
                for hlp in [false, true] {
                    for pieces in [1, 10] {
                        for path in [0, 1, 2] {
                            for wallet_bps in [1_667, 2_000, 5_000] {
                                if !full
                                    && (name != "baseline"
                                        || amp != 4
                                        || !hlp
                                        || pieces != 10
                                        || path != 0
                                        || !controller)
                                {
                                    continue;
                                }
                                let case = Case {
                                    amp,
                                    controller,
                                    hlp,
                                    debt_asset: MarketAsset::Quote,
                                    pieces,
                                    path,
                                    cushion: 200,
                                    withdrawal: 0,
                                };
                                let t = one_case_with_policy(case, Policy { wallet_bps, ..policy });
                                assert_eq!(t.failed_fills, 0, "{name}/{case:?}: {}", t.blocked);
                                assert!(
                                    t.blocked.is_empty() || t.blocked == "entry_margin",
                                    "{name}/{case:?}: {}",
                                    t.blocked
                                );
                                println!("NATIVE_PARAMETERS,{name},{amp},{controller},{hlp},{pieces},{path},{wallet_bps},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                                t.opened,t.original_debt,t.min_entry_health,t.max_entry_requirement,t.partial,t.flash,t.solvent,t.emergency,
                                t.repaid,t.insurance,t.loss,t.owner,t.reward,t.contribution,t.remaining,t.eligible,t.eligible_debt,
                                t.eligible_below_keeper_cost,t.failed_fills,t.blocked);
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(count, if full { 1_080 } else { 3 });
}

#[test]
fn native_cushion_residual_cost_diagnostic() {
    let full = std::env::var_os("DUSK_RESIDUAL_SWEEP").is_some();
    println!("NATIVE_RESIDUAL,amplification,hlp,debt_side,cushion_bps,withdrawal_bps,remaining,eligible,eligible_debt,below_keeper_cost");
    for amp in [1, 4, 10] {
        for hlp in [false, true] {
            for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                for cushion in [0, 200] {
                    for withdrawal in [0, 2_500] {
                        if !full && (amp != 1 || hlp || debt_asset != MarketAsset::Quote || withdrawal != 0) {
                            continue;
                        }
                        let t = one_case(Case {
                            amp,
                            controller: false,
                            hlp,
                            debt_asset,
                            pieces: 10,
                            path: 0,
                            cushion,
                            withdrawal,
                        });
                        println!(
                            "NATIVE_RESIDUAL,{amp},{hlp},{},{cushion},{withdrawal},{},{},{},{}",
                            debt_asset.code(),
                            t.remaining,
                            t.eligible,
                            t.eligible_debt,
                            t.eligible_below_keeper_cost
                        );
                        assert_eq!(t.failed_fills, 0);
                        assert_eq!(
                            t.eligible, t.eligible_below_keeper_cost,
                            "remaining eligibility also has a non-cost obstacle"
                        );
                    }
                }
            }
        }
    }
}
