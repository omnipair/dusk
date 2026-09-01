use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_2022::Token2022,
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION},
    instructions::{HarvestArgs, SetYieldRecipientArgs, WithdrawSingleSidedArgs},
    math::arithmetic::ceil_div,
    program::Dusk,
    state::{FutarchyAuthority, Market, MarketAsset, YieldAccount, YieldTokenKind},
};
use std::cmp::min;

use crate::{constants::*, errors::*, state::*, token::*};

mod cancel;
mod common;
mod create;
mod execute;
mod settle_yield;

pub use cancel::*;
pub use common::*;
pub use create::*;
pub use execute::*;
pub use settle_yield::*;

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/hlp.rs");
}
