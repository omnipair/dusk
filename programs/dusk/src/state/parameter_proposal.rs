use anchor_lang::{prelude::*, solana_program::hash::hashv};

use crate::{
    constants::{
        BPS_DENOMINATOR, MAX_PARAMETER_UPDATES_PER_PROPOSAL, MAX_PROPOSAL_DESCRIPTION_BYTES,
        MAX_PROPOSAL_DESCRIPTION_URI_BYTES, MAX_PROPOSAL_TITLE_BYTES, PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS,
        PARAMETER_PROPOSAL_SPONSOR_BPS, PARAMETER_PROPOSAL_SUPPORT_BPS, PARAMETER_PROPOSAL_TIMELOCK_SECONDS,
        PROPOSAL_METADATA_VERSION,
    },
    errors::ErrorCode,
};

use super::{FeeProfile, IrmConfig, OraclePolicy, TrancheCapital};

/// Immutable support intent. Execution assumptions are committed separately
/// when a majority starts the notice period.
pub const PARAMETER_PROPOSAL_DIGEST_DOMAIN: &[u8] = b"DUSK_PARAMETER_PROPOSAL_V5";
pub const PARAMETER_EXECUTION_DIGEST_DOMAIN: &[u8] = b"DUSK_PARAMETER_EXECUTION_V3";

/// Number of governed parameter families, and of `Market::parameter_revisions`.
pub const PARAMETER_FAMILY_COUNT: usize = 8;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ParameterFamily {
    Fee,
    Concentration,
    Irm,
    EmaHalfLives,
    DailyBorrowLimit,
    CenterController,
    Oracle,
    Tranche,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ProposalOrigin {
    Lp,
    Manager { epoch: u64, policy_version: u64 },
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ProposalClock {
    UnixSeconds,
    Slots,
}

impl ProposalOrigin {
    pub fn clock_kind(self) -> ProposalClock {
        match self {
            Self::Lp => ProposalClock::UnixSeconds,
            Self::Manager { .. } => ProposalClock::Slots,
        }
    }
    pub fn clock_value(self, clock: &Clock) -> Result<i64> {
        match self {
            Self::Lp => Ok(clock.unix_timestamp),
            Self::Manager { .. } => i64::try_from(clock.slot).map_err(|_| error!(ErrorCode::MarketMathOverflow)),
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum GovernanceAction {
    Parameters,
    AppointManager { manager: Pubkey, expected_epoch: u64 },
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct FamilyRevisionCondition {
    pub family: ParameterFamily,
    pub revision: u64,
}

impl ParameterFamily {
    pub const ALL: [Self; PARAMETER_FAMILY_COUNT] = [
        Self::Fee,
        Self::Concentration,
        Self::Irm,
        Self::EmaHalfLives,
        Self::DailyBorrowLimit,
        Self::CenterController,
        Self::Oracle,
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
            Self::Oracle => 6,
            Self::Tranche => 7,
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
    },
    Irm(IrmConfig),
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
    /// Sticky-center movement thresholds. A fully-zero tuple disables center
    /// adjustment in CPMM mode; concentrated markets require a live controller.
    /// Partial zero tuples are invalid.
    CenterController {
        adjustment_threshold_nad: u64,
        adjustment_step_nad: u64,
        min_adjustment_interval_slots: u64,
    },
    /// Prospective reference and blend policy, bounded by the birth config.
    Oracle(OraclePolicy),
    /// Junior underwriting economics. Exit timing remains protocol-defined;
    /// this family changes the income share.
    Tranche {
        junior_income_share_bps: u16,
    },
}

/// One to eight updates, at most one per family, in ascending family order,
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

/// Minimal projection of revisions for the updated families. Collecting
/// endorsements ignore this snapshot; sealing adds every execution dependency.
pub fn updated_family_revisions(
    updates: &[MarketParameterUpdate],
    market_revisions: &[u64; PARAMETER_FAMILY_COUNT],
) -> [u64; PARAMETER_FAMILY_COUNT] {
    let mut revisions = [0; PARAMETER_FAMILY_COUNT];
    for update in updates {
        let index = update.family().code() as usize;
        revisions[index] = market_revisions[index];
    }
    revisions
}

impl MarketParameterUpdate {
    // Oracle policy changes move no inventory and create no debt. An approved
    // standalone policy must remain executable when stale pricing and distress
    // prevent liquidation. Mixed proposals retain ordinary admission guards.
    pub(crate) fn is_oracle_only(updates: &[Self]) -> bool {
        matches!(updates, [Self::Oracle(_)])
    }

    pub fn family(&self) -> ParameterFamily {
        match self {
            Self::Fee(_) => ParameterFamily::Fee,
            Self::Concentration { .. } => ParameterFamily::Concentration,
            Self::Irm(_) => ParameterFamily::Irm,
            Self::EmaHalfLives { .. } => ParameterFamily::EmaHalfLives,
            Self::DailyBorrowLimit { .. } => ParameterFamily::DailyBorrowLimit,
            Self::CenterController { .. } => ParameterFamily::CenterController,
            Self::Oracle(_) => ParameterFamily::Oracle,
            Self::Tranche { .. } => ParameterFamily::Tranche,
        }
    }

    pub fn tranche_notice_required(updates: &[Self]) -> bool {
        updates.iter().any(|update| matches!(update, Self::Tranche { .. }))
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ParameterProposalStatus {
    Collecting,
    Queued,
    Executed,
    Cancelled,
    Expired,
    Stale,
}

impl ParameterProposalStatus {
    pub fn code(self) -> u8 {
        match self {
            Self::Collecting => 0,
            Self::Queued => 1,
            Self::Executed => 2,
            Self::Cancelled => 3,
            Self::Expired => 4,
            Self::Stale => 5,
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct ParameterProposal {
    pub market: Pubkey,
    pub proposer: Pubkey,
    pub nonce: u64,
    /// Cohort whose junior receipt units may contribute to this proposal.
    pub junior_generation: u64,
    pub origin: ProposalOrigin,
    pub action: GovernanceAction,
    pub rent_refund: Pubkey,
    #[max_len(8)]
    pub conditions: Vec<FamilyRevisionCondition>,
    /// The families this proposal changes, one update each, in ascending
    /// family order. They execute together or not at all.
    #[max_len(8)]
    pub updates: Vec<MarketParameterUpdate>,
    /// Full configuration snapshot sealed when a bundle becomes pending.
    /// Collecting endorsements bind only their explicit revision conditions.
    pub family_revisions: [u64; PARAMETER_FAMILY_COUNT],
    pub metadata: ProposalMetadataV1,
    pub digest: [u8; 32],
    pub execution_digest: [u8; 32],
    pub status: ParameterProposalStatus,
    /// Frozen minimum support needed to keep this proposal alive after its
    /// initial 1% sponsorship burn-lock.
    pub sponsorship_floor: u64,
    /// Senior voting units are one-for-one srLP reserve shares.
    pub total_locked: u64,
    /// Current-generation junior receipts. Their voting value is calculated
    /// from the live junior exchange rate whenever support is evaluated.
    pub total_locked_junior_receipts: u64,
    /// Immutable queue-time numerator and direct-srLP denominator.
    pub queued_support: u64,
    pub queued_eligible_srlp: u64,
    pub created_at: i64,
    /// Unix seconds for LP proposals, scheduled slots for manager proposals.
    pub queued_at: i64,
    pub execute_after: i64,
    pub execution_deadline: i64,
    pub bump: u8,
}

impl ParameterProposal {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        market: Pubkey,
        proposer: Pubkey,
        nonce: u64,
        junior_generation: u64,
        market_revisions: &[u64; PARAMETER_FAMILY_COUNT],
        updates: Vec<MarketParameterUpdate>,
        action: GovernanceAction,
        conditions: Vec<FamilyRevisionCondition>,
        metadata: ProposalMetadataV1,
        eligible_supply: u64,
        created_at: i64,
        bump: u8,
    ) -> Result<()> {
        metadata.validate()?;
        match action {
            GovernanceAction::Parameters => validate_parameter_update_set(&updates)?,
            GovernanceAction::AppointManager { manager, .. } => require!(
                updates.is_empty() && manager != Pubkey::default(),
                ErrorCode::InvalidParameterProposal
            ),
        }
        validate_revision_conditions(&conditions)?;
        require!(eligible_supply > 0, ErrorCode::ProposalSponsorshipTooLow);
        let family_revisions = updated_family_revisions(&updates, market_revisions);
        let digest = proposal_intent_digest(
            crate::ID,
            market,
            proposer,
            nonce,
            junior_generation,
            ProposalOrigin::Lp,
            action,
            &conditions,
            &updates,
            &metadata,
        )?;
        self.market = market;
        self.proposer = proposer;
        self.nonce = nonce;
        self.junior_generation = junior_generation;
        self.origin = ProposalOrigin::Lp;
        self.action = action;
        self.rent_refund = proposer;
        self.conditions = conditions;
        self.updates = updates;
        self.family_revisions = family_revisions;
        self.metadata = metadata;
        self.digest = digest;
        self.execution_digest = [0; 32];
        self.status = ParameterProposalStatus::Collecting;
        self.sponsorship_floor = sponsorship_floor(eligible_supply)?;
        self.total_locked = 0;
        self.total_locked_junior_receipts = 0;
        self.queued_support = 0;
        self.queued_eligible_srlp = 0;
        self.created_at = created_at;
        self.queued_at = 0;
        self.execute_after = 0;
        self.execution_deadline = 0;
        self.bump = bump;
        Ok(())
    }

    pub fn assert_account(&self, market: Pubkey, proposal_key: Pubkey) -> Result<()> {
        require_keys_eq!(self.market, market, ErrorCode::InvalidParameterProposal);
        let nonce = self.nonce.to_le_bytes();
        let bump = [self.bump];
        let expected = Pubkey::create_program_address(
            &[
                crate::constants::PARAMETER_PROPOSAL_SEED_PREFIX,
                self.market.as_ref(),
                self.proposer.as_ref(),
                &nonce,
                &bump,
            ],
            &crate::ID,
        )
        .map_err(|_| error!(ErrorCode::InvalidParameterProposal))?;
        require_keys_eq!(proposal_key, expected, ErrorCode::InvalidParameterProposal);
        match self.action {
            GovernanceAction::Parameters => {
                validate_parameter_update_set(&self.updates).map_err(|_| error!(ErrorCode::InvalidParameterProposal))?
            }
            GovernanceAction::AppointManager { manager, .. } => {
                require!(
                    self.origin == ProposalOrigin::Lp && self.updates.is_empty() && manager != Pubkey::default(),
                    ErrorCode::InvalidParameterProposal
                );
            }
        }
        self.assert_digest()?;
        if matches!(
            self.status,
            ParameterProposalStatus::Queued | ParameterProposalStatus::Executed
        ) {
            require!(
                self.execution_digest == self.compute_execution_digest()?,
                ErrorCode::InvalidProposalDigest
            );
        }
        Ok(())
    }

    pub fn assert_digest(&self) -> Result<()> {
        validate_revision_conditions(&self.conditions)?;
        let expected = proposal_intent_digest(
            crate::ID,
            self.market,
            self.proposer,
            self.nonce,
            self.junior_generation,
            self.origin,
            self.action,
            &self.conditions,
            &self.updates,
            &self.metadata,
        )?;
        require!(self.digest == expected, ErrorCode::InvalidProposalDigest);
        Ok(())
    }

    pub fn current_support(&self, tranches: &TrancheCapital) -> Result<u64> {
        let junior = if self.junior_generation == tranches.generation {
            junior_receipt_value(self.total_locked_junior_receipts, tranches)?
        } else {
            0
        };
        self.total_locked
            .checked_add(junior)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn support_is_empty(&self) -> bool {
        self.total_locked == 0 && self.total_locked_junior_receipts == 0
    }

    pub fn queue_if_supported(&mut self, eligible_supply: u64, tranches: &TrancheCapital, now: i64) -> Result<bool> {
        require!(self.origin == ProposalOrigin::Lp, ErrorCode::ProposalNotCollecting);
        require!(
            self.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        let current_support = self.current_support(tranches)?;
        if !has_strict_support_majority(current_support, eligible_supply)? {
            return Ok(false);
        }
        self.queued_support = current_support;
        self.queued_eligible_srlp = eligible_supply;
        self.status = ParameterProposalStatus::Queued;
        self.queued_at = now;
        let timelock = if MarketParameterUpdate::tranche_notice_required(&self.updates) {
            crate::constants::TRANCHE_PARAMETER_TIMELOCK_SECONDS
        } else {
            PARAMETER_PROPOSAL_TIMELOCK_SECONDS
        };
        self.execute_after = now.checked_add(timelock).ok_or(ErrorCode::MarketMathOverflow)?;
        self.execution_deadline = self
            .execute_after
            .checked_add(PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(true)
    }

    /// The families this proposal changes, in ascending order.
    pub fn families(&self) -> impl Iterator<Item = ParameterFamily> + '_ {
        self.updates.iter().map(MarketParameterUpdate::family)
    }

    /// Explicit conditions always apply. Queued proposals additionally require
    /// every dependency sealed when their notice period began.
    pub fn revisions_current(&self, market_revisions: &[u64; PARAMETER_FAMILY_COUNT]) -> bool {
        self.conditions
            .iter()
            .all(|condition| market_revisions[condition.family.code() as usize] == condition.revision)
            && (self.status == ParameterProposalStatus::Collecting || self.family_revisions == *market_revisions)
    }

    pub fn mark_stale_if_revision_changed(&mut self, market_revisions: &[u64; PARAMETER_FAMILY_COUNT]) -> bool {
        if matches!(
            self.status,
            ParameterProposalStatus::Collecting | ParameterProposalStatus::Queued
        ) && !self.revisions_current(market_revisions)
        {
            self.status = ParameterProposalStatus::Stale;
            return true;
        }
        false
    }

    pub fn mark_expired_if_past_deadline(&mut self, now: i64) -> bool {
        if self.status == ParameterProposalStatus::Queued && now > self.execution_deadline {
            self.status = ParameterProposalStatus::Expired;
            return true;
        }
        false
    }

    pub fn cancel_if_below_sponsorship_floor(&mut self, tranches: &TrancheCapital) -> Result<bool> {
        if self.origin == ProposalOrigin::Lp
            && self.status == ParameterProposalStatus::Collecting
            && self.current_support(tranches)? < self.sponsorship_floor
        {
            self.status = ParameterProposalStatus::Cancelled;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn terminal(&self) -> bool {
        matches!(
            self.status,
            ParameterProposalStatus::Executed
                | ParameterProposalStatus::Cancelled
                | ParameterProposalStatus::Expired
                | ParameterProposalStatus::Stale
        )
    }

    pub fn effective_status(
        &self,
        control: &super::GovernanceControl,
        revisions: &[u64; PARAMETER_FAMILY_COUNT],
        key: Pubkey,
        clock: &Clock,
    ) -> ParameterProposalStatus {
        if self.terminal() {
            return self.status;
        }
        let authority_current = match self.origin {
            ProposalOrigin::Lp => match self.action {
                GovernanceAction::Parameters => true,
                GovernanceAction::AppointManager { expected_epoch, .. } => expected_epoch == control.manager_epoch,
            },
            ProposalOrigin::Manager { epoch, policy_version } => {
                control.manager == self.proposer
                    && control.manager_epoch == epoch
                    && policy_version == super::MANAGER_POLICY_VERSION
            }
        };
        if !authority_current || !self.revisions_current(revisions) {
            return ParameterProposalStatus::Stale;
        }
        if self.status == ParameterProposalStatus::Queued {
            let now = match self.origin.clock_value(clock) {
                Ok(now) => now,
                Err(_) => return ParameterProposalStatus::Expired,
            };
            if now > self.execution_deadline {
                return ParameterProposalStatus::Expired;
            }
            if !control.pending.is_some_and(|pending| pending.proposal == key) {
                return ParameterProposalStatus::Stale;
            }
        }
        self.status
    }

    pub fn refresh_intent_digest(&mut self) -> Result<()> {
        validate_revision_conditions(&self.conditions)?;
        self.digest = proposal_intent_digest(
            crate::ID,
            self.market,
            self.proposer,
            self.nonce,
            self.junior_generation,
            self.origin,
            self.action,
            &self.conditions,
            &self.updates,
            &self.metadata,
        )?;
        Ok(())
    }

    pub fn seal_execution(&mut self, revisions: &[u64; PARAMETER_FAMILY_COUNT]) -> Result<()> {
        require!(
            self.status == ParameterProposalStatus::Queued && self.execution_digest == [0; 32],
            ErrorCode::InvalidParameterProposal
        );
        require!(
            self.conditions
                .iter()
                .all(|condition| revisions[condition.family.code() as usize] == condition.revision),
            ErrorCode::ProposalStale
        );
        self.family_revisions = *revisions;
        self.execution_digest = self.compute_execution_digest()?;
        Ok(())
    }

    pub fn pending_bundle(&self, proposal: Pubkey) -> super::PendingBundle {
        super::PendingBundle {
            proposal,
            origin: self.origin,
            action: self.action,
            deadline: self.execution_deadline,
        }
    }

    fn compute_execution_digest(&self) -> Result<[u8; 32]> {
        let snapshot = (
            self.family_revisions,
            self.queued_support,
            self.queued_eligible_srlp,
            self.queued_at,
            self.execute_after,
            self.execution_deadline,
        )
            .try_to_vec()?;
        Ok(hashv(&[PARAMETER_EXECUTION_DIGEST_DOMAIN, &self.digest, &snapshot]).to_bytes())
    }
}

pub fn junior_receipt_value(receipts: u64, tranches: &TrancheCapital) -> Result<u64> {
    if receipts == 0 || tranches.junior_reserve_shares == 0 {
        return Ok(0);
    }
    require!(tranches.junior_receipt_supply > 0, ErrorCode::BrokenInvariant);
    u64::try_from(
        (receipts as u128)
            .checked_mul(tranches.junior_reserve_shares as u128)
            .and_then(|value| value.checked_div(tranches.junior_receipt_supply as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub fn sponsorship_floor(eligible_supply: u64) -> Result<u64> {
    let numerator = (eligible_supply as u128)
        .checked_mul(PARAMETER_PROPOSAL_SPONSOR_BPS as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = BPS_DENOMINATOR as u128;
    let rounded = numerator
        .checked_add(denominator - 1)
        .and_then(|value| value.checked_div(denominator))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(rounded).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub fn has_strict_support_majority(total_locked: u64, eligible_supply: u64) -> Result<bool> {
    let support = (total_locked as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let threshold = (eligible_supply as u128)
        .checked_mul(PARAMETER_PROPOSAL_SUPPORT_BPS as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(support > threshold)
}

pub fn validate_revision_conditions(conditions: &[FamilyRevisionCondition]) -> Result<()> {
    require!(
        conditions.len() <= PARAMETER_FAMILY_COUNT
            && conditions
                .windows(2)
                .all(|pair| pair[0].family.code() < pair[1].family.code()),
        ErrorCode::ParameterUpdatesNotCanonical
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn parameter_proposal_digest(
    program_id: Pubkey,
    market: Pubkey,
    proposer: Pubkey,
    nonce: u64,
    junior_generation: u64,
    _family_revisions: &[u64; PARAMETER_FAMILY_COUNT],
    updates: &[MarketParameterUpdate],
    metadata: &ProposalMetadataV1,
) -> Result<[u8; 32]> {
    proposal_intent_digest(
        program_id,
        market,
        proposer,
        nonce,
        junior_generation,
        ProposalOrigin::Lp,
        GovernanceAction::Parameters,
        &[],
        updates,
        metadata,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn proposal_intent_digest(
    program_id: Pubkey,
    market: Pubkey,
    proposer: Pubkey,
    nonce: u64,
    junior_generation: u64,
    origin: ProposalOrigin,
    action: GovernanceAction,
    conditions: &[FamilyRevisionCondition],
    updates: &[MarketParameterUpdate],
    metadata: &ProposalMetadataV1,
) -> Result<[u8; 32]> {
    let payload = (origin, action, conditions.to_vec(), updates.to_vec(), metadata.clone()).try_to_vec()?;
    Ok(hashv(&[
        PARAMETER_PROPOSAL_DIGEST_DOMAIN,
        program_id.as_ref(),
        market.as_ref(),
        proposer.as_ref(),
        &nonce.to_le_bytes(),
        &junior_generation.to_le_bytes(),
        &payload,
    ])
    .to_bytes())
}

#[cfg(test)]
mod tests {
    include!("../tests/state/parameter_proposal.rs");
}
