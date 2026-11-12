//! Native tranche accounting and controller operations. Instruction handlers
//! must authenticate ownership and settle custody around these checked updates.

pub use crate::state::{
    IncomeExposureEnd, IncomeReceivableBook, IncomeReceivableClaim, JuniorExitConfig, JuniorExitTicket,
    LazyIncomeBooks, MarketIncomeBooks, ReceivableGrowth, RiskRateBook, RiskRateConfig, RiskRateOverrides,
    RiskRateSide, RiskRateUpdate, TrancheCapital, TrancheIncome, TranchePolicy, TrancheRevenueCarry,
};

mod junior_exit;
mod risk_rate_book;
mod risk_rate_model;
mod tranche_capital;
mod tranche_revenue;

pub use risk_rate_model::*;
pub use tranche_capital::*;
pub use tranche_revenue::*;

mod market;

mod tranche_income;

mod income_receivable;

mod market_income;
mod seed;

mod lazy_income;
