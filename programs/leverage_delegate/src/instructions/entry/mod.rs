use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_2022::Token2022,
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, LEVERAGE_MAX_MULTIPLIER_BPS, MARKET_LAYOUT_VERSION},
    instructions::{leverage_position_pda, OpenLeverageArgs, LEVERAGE_HLP_ACCOUNT_PREFIX_LEN},
    program::Dusk,
    state::{
        FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner,
    },
};

use crate::{constants::*, errors::*, state::*, token::*};

mod cancel;
mod common;
mod create;
mod execute;

pub use cancel::*;
pub use common::*;
pub use create::*;
pub use execute::*;

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/entry.rs");
}
