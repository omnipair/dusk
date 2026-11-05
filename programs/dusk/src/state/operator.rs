use crate::{
    constants::*,
    errors::ErrorCode,
    state::{MarketParameterUpdate, ProposalMetadataV1, PARAMETER_FAMILY_COUNT},
};
use anchor_lang::{prelude::*, solana_program::hash::hashv};

pub const OPERATOR_PROPOSAL_SEED: &[u8] = b"operator_change";
#[constant]
pub const OPERATOR_POLICY_VERSION: u64 = 1;
#[constant]
pub const OPERATOR_NOTICE_SLOTS: u64 = JUNIOR_MAX_NOTICE_SLOTS + MS_PER_DAY / TARGET_MS_PER_SLOT;
#[constant]
pub const OPERATOR_EXECUTION_WINDOW_SLOTS: u64 = 7 * MS_PER_DAY / TARGET_MS_PER_SLOT;
#[constant]
pub const MAX_OPERATOR_INTEREST_SHARE_BPS: u16 = 5_000;

/// Operator authority is independent from LP ownership. A governance program
/// can hold it using a signer PDA; voting and token issuance belong outside Dusk.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct OperatorState {
    pub authority: Pubkey,
    pub epoch: u64,
    pub revenue_epoch: u64,
    /// Monotone across authority transfers; a closed proposal PDA cannot be reused.
    pub next_nonce: u64,
    pub pending: Pubkey,
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
    pub fn assert_authority(&self, authority: Pubkey) -> Result<()> {
        require!(authority != Pubkey::default(), ErrorCode::InvalidOperator);
        require_keys_eq!(authority, self.authority, ErrorCode::InvalidOperator);
        Ok(())
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
            self.next_operator,
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
        Ok(hashv(&[b"DUSK_OPERATOR_CHANGE_V1", crate::ID.as_ref(), &payload]).to_bytes())
    }
    pub fn current(&self, market: &crate::state::Market, key: Pubkey) -> bool {
        !self.executed
            && market.operator.pending == key
            && market.operator.authority == self.operator
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
    pub start: crate::transitions::amm::ConcentratedCurveParameters,
    pub target: crate::transitions::amm::ConcentratedCurveParameters,
    pub start_slot: u64,
    pub end_slot: u64,
    /// Successful or rejected steps are attempted at most once per slot.
    pub last_attempt_slot: u64,
}
impl ConcentrationRamp {
    pub fn validate_target(
        start: crate::transitions::amm::ConcentratedCurveParameters,
        target: crate::transitions::amm::ConcentratedCurveParameters,
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
    pub fn candidate(&self, slot: u64) -> Result<crate::transitions::amm::ConcentratedCurveParameters> {
        require!(
            self.active && self.end_slot > self.start_slot,
            ErrorCode::BrokenInvariant
        );
        if slot >= self.end_slot {
            return Ok(self.target);
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
        Ok(crate::transitions::amm::ConcentratedCurveParameters {
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
        })
    }
}
