use anchor_lang::prelude::*;

use crate::state::{FeeProfile, IrmUpdate};
use crate::{constants::*, errors::ErrorCode};

/// Number of configurable parameter families, and of `Market::parameter_revisions`.
pub const PARAMETER_FAMILY_COUNT: usize = 7;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ParameterFamily {
    Fee,
    Concentration,
    Irm,
    EmaHalfLives,
    DailyBorrowLimit,
    CenterController,
    Tranche,
}

impl ParameterFamily {
    pub const ALL: [Self; PARAMETER_FAMILY_COUNT] = [
        Self::Fee,
        Self::Concentration,
        Self::Irm,
        Self::EmaHalfLives,
        Self::DailyBorrowLimit,
        Self::CenterController,
        Self::Tranche,
    ];
    pub fn code(self) -> u8 {
        match self {
            Self::Fee => 0,
            Self::Concentration => 1,
            Self::Irm => 2,
            Self::EmaHalfLives => 3,
            Self::DailyBorrowLimit => 4,
            Self::CenterController => 5,
            Self::Tranche => 6,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, InitSpace, PartialEq, Eq)]
pub enum MarketParameterUpdate {
    Fee(FeeProfile),
    Concentration {
        peak_amplification_nad: u64,
        core_half_width_bps: u16,
        fade_width_bps: u16,
        ramp_duration_slots: u64,
    },
    Irm(IrmUpdate),
    EmaHalfLives {
        price_ms: u64,
        directional_price_ms: u64,
        curve_depth_ms: u64,
        center_price_ms: u64,
    },
    /// Rolling limit for principal lent out through the public borrow path.
    DailyBorrowLimit {
        max_daily_borrow_bps: u16,
    },
    /// Minimum adaptive centering step. Concentrated markets always retain
    /// the EMA controller and its funded-profit admission check.
    CenterController {
        adjustment_step_nad: u64,
    },
    /// Junior underwriting economics. Exit timing remains protocol-defined;
    /// this family changes the income share.
    Tranche {
        junior_income_share_bps: u16,
    },
}

/// One to seven updates, at most one per family, in ascending family order,
/// so a proposal's updates have exactly one encoding and one digest.
pub fn validate_parameter_update_set(updates: &[MarketParameterUpdate]) -> Result<()> {
    require!(
        !updates.is_empty() && updates.len() <= MAX_PARAMETER_UPDATES_PER_PROPOSAL,
        ErrorCode::ParameterUpdatesNotCanonical
    );
    require!(
        updates
            .windows(2)
            .all(|pair| pair[0].family().code() < pair[1].family().code()),
        ErrorCode::ParameterUpdatesNotCanonical
    );
    Ok(())
}

impl MarketParameterUpdate {
    pub fn family(&self) -> ParameterFamily {
        match self {
            Self::Fee(_) => ParameterFamily::Fee,
            Self::Concentration { .. } => ParameterFamily::Concentration,
            Self::Irm(_) => ParameterFamily::Irm,
            Self::EmaHalfLives { .. } => ParameterFamily::EmaHalfLives,
            Self::DailyBorrowLimit { .. } => ParameterFamily::DailyBorrowLimit,
            Self::CenterController { .. } => ParameterFamily::CenterController,
            Self::Tranche { .. } => ParameterFamily::Tranche,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, InitSpace, PartialEq, Eq)]
pub struct ProposalMetadataV1 {
    pub version: u8,
    #[max_len(96)]
    pub title: String,
    #[max_len(200)]
    pub description_uri: String,
    pub description_sha256: [u8; 32],
    pub description_len: u32,
}

impl ProposalMetadataV1 {
    pub fn validate(&self) -> Result<()> {
        require_eq!(
            self.version,
            PROPOSAL_METADATA_VERSION,
            ErrorCode::InvalidProposalMetadata
        );
        require!(
            !self.title.is_empty()
                && self.title.len() <= MAX_PROPOSAL_TITLE_BYTES
                && self.title == self.title.trim()
                && !self.title.chars().any(char::is_control),
            ErrorCode::InvalidProposalMetadata
        );
        require!(
            !self.description_uri.is_empty()
                && self.description_uri.len() <= MAX_PROPOSAL_DESCRIPTION_URI_BYTES
                && self.description_uri.is_ascii()
                && !self
                    .description_uri
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()),
            ErrorCode::InvalidProposalUri
        );
        let uri_payload = self
            .description_uri
            .strip_prefix("ipfs://")
            .or_else(|| self.description_uri.strip_prefix("ar://"))
            .or_else(|| self.description_uri.strip_prefix("https://"));
        require!(
            uri_payload.is_some_and(|payload| !payload.is_empty()),
            ErrorCode::InvalidProposalUri
        );
        require!(
            (1..=MAX_PROPOSAL_DESCRIPTION_BYTES).contains(&self.description_len) && self.description_sha256 != [0; 32],
            ErrorCode::InvalidProposalMetadata
        );
        Ok(())
    }
}
