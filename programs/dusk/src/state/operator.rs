use crate::{
    constants::*,
    errors::ErrorCode,
    state::{FeeProfile, IrmUpdate},
};
use anchor_lang::{prelude::*, solana_program::hash::hashv};

pub const OPERATOR_PROPOSAL_SEED: &[u8] = b"operator_change";
#[constant]
pub const OPERATOR_POLICY_VERSION: u64 = 2;
#[constant]
pub const OPERATOR_NOTICE_SLOTS: u64 = JUNIOR_MAX_NOTICE_SLOTS + MS_PER_DAY / TARGET_MS_PER_SLOT;
#[constant]
pub const OPERATOR_EXECUTION_WINDOW_SLOTS: u64 = 7 * MS_PER_DAY / TARGET_MS_PER_SLOT;
#[constant]
pub const MAX_OPERATOR_INTEREST_SHARE_BPS: u16 = 5_000;

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

/// Operator authority is independent from LP ownership. A governance program
/// can hold it using a signer PDA; voting and token issuance belong outside Dusk.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct OperatorState {
    pub authority: Pubkey,
    /// Optional parameter delegate; operator ownership and revenue stay with authority.
    pub manager: Pubkey,
    pub epoch: u64,
    pub revenue_epoch: u64,
    /// Monotone across authority transfers; a closed proposal PDA cannot be reused.
    pub next_nonce: u64,
    pub pending: Pubkey,
    /// Prevents a manager from replacing an operator-authored bundle.
    pub pending_author: Pubkey,
    pub revenue_recipient: Pubkey,
    pub interest_share_bps: u16,
}

impl OperatorState {
    pub fn new(deployer: Pubkey) -> Self {
        Self {
            authority: deployer,
            revenue_recipient: deployer,
            ..Self::default()
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct OperatorProposal {
    pub market: Pubkey,
    pub operator: Pubkey,
    pub epoch: u64,
    pub nonce: u64,
    pub policy_version: u64,
    #[max_len(7)]
    pub updates: Vec<MarketParameterUpdate>,
    pub next_operator: Option<Pubkey>,
    /// Some(zero) clears the manager; None leaves it unchanged.
    pub next_manager: Option<Pubkey>,
    pub revenue_recipient: Option<Pubkey>,
    pub interest_share_bps: Option<u16>,
    pub metadata: ProposalMetadataV1,
    pub family_revisions: [u64; PARAMETER_FAMILY_COUNT],
    pub announced_at_slot: u64,
    pub execute_after_slot: u64,
    pub deadline_slot: u64,
    pub rent_refund: Pubkey,
    pub digest: [u8; 32],
    pub executed: bool,
    pub bump: u8,
}

impl OperatorProposal {
    pub fn intent_digest(&self) -> Result<[u8; 32]> {
        let payload = (
            self.market,
            self.operator,
            self.epoch,
            self.nonce,
            self.policy_version,
            &self.updates,
            (self.next_operator, self.next_manager),
            self.revenue_recipient,
            self.interest_share_bps,
            &self.metadata,
            self.family_revisions,
            self.announced_at_slot,
            self.execute_after_slot,
            self.deadline_slot,
            self.rent_refund,
        )
            .try_to_vec()?;
        Ok(hashv(&[b"DUSK_OPERATOR_CHANGE_V2", crate::ID.as_ref(), &payload]).to_bytes())
    }
    pub fn current(&self, market: &crate::state::Market, key: Pubkey) -> bool {
        !self.executed
            && market.operator.pending == key
            && market.operator.authority != Pubkey::default()
            && (market.operator.authority == self.operator || market.operator.manager == self.operator)
            && self.operator != Pubkey::default()
            && market.operator.pending_author == self.operator
            && market.operator.epoch == self.epoch
            && self.policy_version == OPERATOR_POLICY_VERSION
            && market.parameter_revisions == self.family_revisions
    }
    pub fn assert_ready(&self, market: &crate::state::Market, key: Pubkey, slot: u64) -> Result<()> {
        require!(self.current(market, key), ErrorCode::OperatorProposalStale);
        require_gte!(slot, self.execute_after_slot, ErrorCode::OperatorNoticeNotReady);
        require!(slot <= self.deadline_slot, ErrorCode::OperatorProposalExpired);
        require!(self.digest == self.intent_digest()?, ErrorCode::OperatorProposalStale);
        Ok(())
    }
}

#[constant]
pub const MIN_CONCENTRATION_RAMP_SLOTS: u64 = MS_PER_DAY / TARGET_MS_PER_SLOT;

/// Scheduled targets do not mutate live geometry until a guarded step succeeds.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ConcentrationRamp {
    pub active: bool,
    pub start: crate::state::ConcentratedCurveParameters,
    pub target: crate::state::ConcentratedCurveParameters,
    pub start_slot: u64,
    pub end_slot: u64,
    /// Successful or rejected steps are attempted at most once per slot.
    pub last_attempt_slot: u64,
}
impl ConcentrationRamp {
    pub fn validate_target(
        start: crate::state::ConcentratedCurveParameters,
        target: crate::state::ConcentratedCurveParameters,
        duration: u64,
    ) -> Result<()> {
        start.validate(crate::state::MAX_AMM_AMPLIFICATION_NAD)?;
        target.validate(crate::state::MAX_AMM_AMPLIFICATION_NAD)?;
        require_gte!(
            duration,
            MIN_CONCENTRATION_RAMP_SLOTS,
            ErrorCode::OperatorPolicyExceeded
        );
        for (from, to) in [
            (start.peak_amplification_nad, target.peak_amplification_nad),
            (start.core_half_width_bps as u64, target.core_half_width_bps as u64),
            (start.fade_width_bps as u64, target.fade_width_bps as u64),
        ] {
            // Zero-width mode transitions cannot use a multiplicative bound.
            // They still receive full notice, interpolation and every guard.
            if from != 0 && to != 0 {
                require!(
                    to as u128 <= from as u128 * 10 && from as u128 <= to as u128 * 10,
                    ErrorCode::OperatorPolicyExceeded
                );
            }
        }
        Ok(())
    }
    pub fn candidate(&self, slot: u64) -> Result<crate::state::ConcentratedCurveParameters> {
        require!(
            self.active && self.end_slot > self.start_slot,
            ErrorCode::BrokenInvariant
        );
        if slot >= self.end_slot {
            return Ok(self.target);
        }
        if slot <= self.start_slot {
            return Ok(self.start);
        }
        let elapsed = slot.saturating_sub(self.start_slot) as u128;
        let duration = (self.end_slot - self.start_slot) as u128;
        let lerp = |a: u64, b: u64| -> u64 {
            if b >= a {
                a + ((b - a) as u128 * elapsed / duration) as u64
            } else {
                a - ((a - b) as u128 * elapsed / duration) as u64
            }
        };
        let amplification = lerp(self.start.peak_amplification_nad, self.target.peak_amplification_nad);
        let cpmm = amplification == NAD;
        let mut candidate = crate::state::ConcentratedCurveParameters {
            peak_amplification_nad: amplification,
            core_half_width_bps: if cpmm {
                0
            } else {
                lerp(
                    self.start.core_half_width_bps as u64,
                    self.target.core_half_width_bps as u64,
                )
                .max(1) as u16
            },
            fade_width_bps: if cpmm {
                0
            } else {
                lerp(self.start.fade_width_bps as u64, self.target.fade_width_bps as u64) as u16
            },
        };
        if !cpmm && candidate.validate(crate::state::MAX_AMM_AMPLIFICATION_NAD).is_err() {
            // Linear widths/amplification can cross the invalid zero-tail
            // region even when both announced endpoints are valid. Preserve
            // linear amplification wherever possible and cap only at the
            // maximum supported by these intermediate widths and tail floor.
            candidate.fade_width_bps = candidate.fade_width_bps.min(u16::MAX - candidate.core_half_width_bps);
            let (core, outer) = candidate.sqrt_widths_nad()?;
            let inverse_core = crate::math::mul_div_u128(NAD as u128, NAD as u128, core)?;
            let inverse_width = if candidate.fade_width_bps == 0 {
                inverse_core
            } else {
                (inverse_core + crate::math::mul_div_u128(NAD as u128, NAD as u128, outer)?) / 2
            };
            let maximum_concentrated_share = u128::from(NAD - crate::state::MIN_AMM_TAIL_SHARE_NAD);
            let minimum_inverse_amplification =
                u128::from(NAD) - crate::math::mul_div_u128(maximum_concentrated_share, inverse_width, NAD as u128)?;
            let maximum_amplification =
                crate::math::mul_div_u128(NAD as u128, NAD as u128, minimum_inverse_amplification)?;
            candidate.peak_amplification_nad = amplification.min(maximum_amplification as u64);
        }
        candidate.validate(crate::state::MAX_AMM_AMPLIFICATION_NAD)?;
        Ok(candidate)
    }
}
