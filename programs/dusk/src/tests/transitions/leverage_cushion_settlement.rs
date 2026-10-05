#[derive(Clone, Copy)]
pub(super) struct Fill {
    pub kind: u8, // partial purchase, full purchase, solvent internal, emergency
    pub sold: u64,
    pub output: u64,
    pub payment: u64,
    pub contribution: u64,
    pub reward: u64,
}

#[derive(Default)]
pub(super) struct Paid {
    pub repaid: u64,
    pub insurance: u64,
    pub loss: u64,
    pub owner: u64,
    pub reward: u64,
    pub contribution: u64,
}

pub(super) fn sale(market: &Market, asset: MarketAsset, amount: u64, slot: u64) -> Result<(Market, u64)> {
    let mut trial = market.clone();
    no_accrual(&mut trial, slot);
    let mut prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: (slot * 400 / 1_000) as i64,
        asset_in: asset,
        reserve_credit: amount,
        protocol_fee_bps: 0,
    }
    .prepare(&mut trial)?;
    let output = prepared.quote.amount_out;
    prepared.finalize_state(&mut trial, slot, 0, ProtocolAuctionSplit::default())?;
    Ok((trial, output))
}

pub(super) fn choose_fill(
    market: &Market,
    position: &LeveragePosition,
    terms: &LeverageMarginSchedule,
    cushion: u64,
    slot: u64,
) -> Option<Fill> {
    let asset = position.collateral_asset().unwrap();
    let debt = position.debt_amount(&market.debt).unwrap();
    let value = reference_credit(market, asset, position.collateral_amount);
    let h = equity_bps(value, debt).unwrap() as u64;
    let mm = terms
        .effective_maintenance_bps(position.collateral_amount.into())
        .unwrap() as u64;
    if h > mm {
        return None;
    }
    let severity = (mm - h) * 10_000 / mm;
    let discount = 50 + 450 * severity / 10_000;
    let mut partial = None;
    // Same 1% grid as the Python experiment; absence is not an optimality proof.
    for bps in (100..10_000).step_by(100) {
        let sold = position.collateral_amount * bps / 10_000;
        if sold == 0 {
            continue;
        }
        let Ok((_, output)) = sale(market, asset, sold, slot) else {
            continue;
        };
        let sold_value = reference_credit(market, asset, sold);
        let payment = (u128::from(sold_value) * u128::from(10_000 - discount)).div_ceil(10_000) as u64;
        let contribution = payment * 20 / 10_000;
        let repay = payment - contribution;
        if repay == 0 || repay >= debt || output < payment + 50 {
            continue;
        }
        let left = position.collateral_amount - sold;
        let after_value = reference_credit(market, asset, left);
        if after_value <= debt - repay {
            continue;
        }
        let after = equity_bps(after_value, debt - repay).unwrap() as u64;
        let after_mm = terms.effective_maintenance_bps(left.into()).unwrap() as u64;
        if after <= h || after + mm <= h + after_mm {
            continue;
        }
        partial = Some(Fill {
            kind: 0,
            sold,
            output,
            payment,
            contribution,
            reward: 0,
        });
        if after >= after_mm + 200 {
            return partial;
        }
    }
    if partial.is_some() {
        return partial;
    }
    let sold = position.collateral_amount;
    let Ok((_, output)) = sale(market, asset, sold, slot) else {
        return None;
    };
    let payment = (u128::from(value) * u128::from(10_000 - discount)).div_ceil(10_000) as u64;
    if output >= payment + 50 {
        return Some(Fill {
            kind: 1,
            sold,
            output,
            payment,
            contribution: payment.saturating_sub(debt).min(debt * 20 / 10_000),
            reward: 0,
        });
    }
    let reward = output * severity / 1_000_000; // up to 1% of actual proceeds
    let payment = output - reward;
    let contribution = payment.saturating_sub(debt).min(debt * 20 / 10_000);
    if reward >= 50 && cushion > 0 && payment >= debt && (payment - debt - contribution) * 10_000 <= debt * cushion {
        return Some(Fill {
            kind: 2,
            sold,
            output,
            payment,
            contribution,
            reward,
        });
    }
    if h * 2 <= mm && reward >= 50 {
        return Some(Fill {
            kind: 3,
            sold,
            output,
            payment,
            contribution,
            reward,
        });
    }
    None
}

pub(super) fn settle(
    market: &Market,
    position: &LeveragePosition,
    fill: Fill,
    slot: u64,
) -> Result<(Market, LeveragePosition, Paid)> {
    // Fork both accounts. A rejected hypothetical settlement commits neither.
    // This is economic native composition; it is not a flash-session/CPI test.
    let asset = position.collateral_asset()?;
    let debt_asset = asset.opposite();
    let (mut trial, actual) = sale(market, asset, fill.sold, slot)?;
    require_eq!(actual, fill.output, ErrorCode::BrokenInvariant);
    require_gte!(actual, fill.payment + fill.reward, ErrorCode::BrokenInvariant);
    let mut next = position.clone();
    let debt = next.debt_amount(&trial.debt)?;
    require_eq!(u128::from(debt), next.debt_principal, ErrorCode::BrokenInvariant);
    let recovery = fill.payment - fill.contribution;
    if fill.kind == 2 {
        require_gte!(recovery, debt, ErrorCode::InsufficientAmount);
    }
    let repaid = recovery.min(debt);
    if repaid > 0 {
        trial.repay_leverage_debt(&mut next, repaid, slot)?;
    }
    let mut paid = Paid {
        repaid,
        owner: recovery.saturating_sub(debt),
        reward: fill.reward,
        contribution: fill.contribution,
        ..Paid::default()
    };
    if fill.contribution > 0 {
        trial.insurance.credit(debt_asset, fill.contribution, slot)?;
    }
    if fill.kind > 0 && next.debt_shares > 0 {
        // All sold collateral has left in this atomic model step. Keep its
        // original field until the native zero-credit terminal adapter clears
        // it, so require_open and final write-off see the original obligation.
        let remaining = next.debt_amount(&trial.debt)?;
        let insurance = trial.insurance.draw_capacity(debt_asset, slot)?.min(remaining);
        let result = trial.liquidate_leverage_position(
            &mut next,
            None,
            0,
            LeverageSwapFeeCredit::default(),
            LeverageInsuranceDraw {
                spent: insurance,
                credit: insurance,
            },
            0,
            ProtocolAuctionSplit::default(),
            slot,
        )?;
        paid.insurance = result.insurance_drawn;
        paid.loss = result.principal_written_off;
    } else {
        next.collateral_amount -= fill.sold;
    }
    if fill.kind > 0 {
        require_eq!(next.debt_shares, 0, ErrorCode::BrokenInvariant);
        require_eq!(next.debt_principal, 0, ErrorCode::BrokenInvariant);
        require_eq!(next.collateral_amount, 0, ErrorCode::BrokenInvariant);
    }
    if fill.kind == 2 {
        require_eq!(paid.insurance, 0, ErrorCode::BrokenInvariant);
        require_eq!(paid.loss, 0, ErrorCode::BrokenInvariant);
    }
    trial.assert_market_invariants()?;
    require_eq!(trial.base_hlp_vault.residual_exposure, 0, ErrorCode::BrokenInvariant);
    require_eq!(trial.quote_hlp_vault.residual_exposure, 0, ErrorCode::BrokenInvariant);
    Ok((trial, next, paid))
}
