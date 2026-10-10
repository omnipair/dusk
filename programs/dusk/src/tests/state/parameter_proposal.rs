use super::*;
use crate::constants::{MAX_PARAMETER_UPDATES_PER_PROPOSAL, PARAMETER_PROPOSAL_SEED_PREFIX, YIELD_GROWTH_SCALE_Q64};
use crate::instructions::CreateParameterProposalArgs;
use crate::state::{ProposalSupport, VirtualYieldLedger, YieldAccount, YieldTokenKind};
use anchor_lang::{InstructionData, ToAccountMetas};

fn metadata() -> ProposalMetadataV1 {
    ProposalMetadataV1 {
        version: PROPOSAL_METADATA_VERSION,
        title: "Raise the daily borrow limit".to_string(),
        description_uri: "ipfs://bafy-test".to_string(),
        description_sha256: [7; 32],
        description_len: 512,
    }
}

fn revisions(values: [u64; PARAMETER_FAMILY_COUNT]) -> [u64; PARAMETER_FAMILY_COUNT] {
    values
}

/// One update per family, in ascending family order: the largest set a
/// proposal can carry.
fn every_family_update() -> Vec<MarketParameterUpdate> {
    vec![
        MarketParameterUpdate::Fee(FeeProfile::default()),
        MarketParameterUpdate::Concentration {
            peak_amplification_nad: 0,
            core_half_width_bps: 0,
            fade_width_bps: 0,
        },
        MarketParameterUpdate::Irm(IrmConfig::default()),
        MarketParameterUpdate::EmaHalfLives {
            price_ms: 0,
            directional_price_ms: 0,
            curve_depth_ms: 0,
            center_price_ms: 0,
        },
        MarketParameterUpdate::DailyBorrowLimit {
            max_daily_borrow_bps: 0,
        },
        MarketParameterUpdate::CenterController {
            adjustment_threshold_nad: 0,
            adjustment_step_nad: 0,
            min_adjustment_interval_slots: 0,
        },
        MarketParameterUpdate::InsuranceDrawCaps {
            principal_coverage_bps: 0,
            per_day_bps: 0,
        },
    ]
}

fn empty_yield_account() -> YieldAccount {
    YieldAccount {
        harvest_authority: None,
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: YieldTokenKind::Ylp.code(),
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    }
}

#[test]
fn metadata_enforces_canonical_title_and_supported_uri() {
    metadata().validate().unwrap();

    let mut invalid_title = metadata();
    invalid_title.title.push(' ');
    assert!(invalid_title.validate().is_err());

    let mut invalid_uri = metadata();
    invalid_uri.description_uri = "http://example.com/proposal".to_string();
    assert!(invalid_uri.validate().is_err());

    let mut https_uri = metadata();
    https_uri.description_uri = "https://example.com/proposal.json".to_string();
    https_uri.validate().unwrap();
}

#[test]
fn metadata_enforces_every_serialized_bound_in_bytes() {
    let mut value = metadata();
    value.title = "é".repeat(MAX_PROPOSAL_TITLE_BYTES / 2);
    value.description_uri = format!(
        "https://{}",
        "a".repeat(MAX_PROPOSAL_DESCRIPTION_URI_BYTES - "https://".len())
    );
    value.description_len = MAX_PROPOSAL_DESCRIPTION_BYTES;
    value.validate().unwrap();

    value.title.push('é');
    assert!(value.validate().is_err());
    value.title = "valid".to_string();
    value.description_uri.push('a');
    assert!(value.validate().is_err());
    value.description_uri = "ar://transaction".to_string();
    value.description_len = MAX_PROPOSAL_DESCRIPTION_BYTES + 1;
    assert!(value.validate().is_err());
    value.description_len = 1;
    value.description_sha256 = [0; 32];
    assert!(value.validate().is_err());
}

#[test]
fn digest_binds_nonce_revisions_updates_and_metadata() {
    let program = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let proposer = Pubkey::new_unique();
    let borrow_limit = MarketParameterUpdate::DailyBorrowLimit {
        max_daily_borrow_bps: 1_000,
    };
    let caps = MarketParameterUpdate::InsuranceDrawCaps {
        principal_coverage_bps: 100,
        per_day_bps: 200,
    };
    let updates = vec![borrow_limit.clone(), caps.clone()];
    let base_revisions = revisions([0, 0, 0, 0, 9, 0, 3]);
    let digest = |nonce, family_revisions: &[u64; PARAMETER_FAMILY_COUNT], updates: &[MarketParameterUpdate], metadata: &ProposalMetadataV1| {
        parameter_proposal_digest(program, market, proposer, nonce, family_revisions, updates, metadata).unwrap()
    };
    let expected = digest(4, &base_revisions, &updates, &metadata());

    assert_ne!(expected, digest(5, &base_revisions, &updates, &metadata()));
    assert_ne!(expected, digest(4, &revisions([0, 0, 0, 0, 10, 0, 3]), &updates, &metadata()));
    assert_ne!(expected, digest(4, &revisions([0, 0, 0, 0, 9, 0, 4]), &updates, &metadata()));
    assert_ne!(expected, digest(4, &base_revisions, &[borrow_limit], &metadata()));
    assert_ne!(expected, digest(4, &base_revisions, &[caps.clone()], &metadata()));
    assert_ne!(
        expected,
        digest(
            4,
            &base_revisions,
            &[
                MarketParameterUpdate::DailyBorrowLimit {
                    max_daily_borrow_bps: 1_001,
                },
                caps,
            ],
            &metadata()
        )
    );

    for byte_index in 0..32 {
        let mut changed = metadata();
        changed.description_sha256[byte_index] ^= 1;
        assert_ne!(
            expected,
            digest(4, &base_revisions, &updates, &changed),
            "description hash byte {byte_index} was not bound"
        );
    }

    let mut changed_title = metadata();
    changed_title.title = "Lower the daily borrow limit".to_string();
    assert_ne!(expected, digest(4, &base_revisions, &updates, &changed_title));
}

#[test]
fn update_set_names_each_family_once_in_ascending_order() {
    validate_parameter_update_set(&every_family_update()).unwrap();
    validate_parameter_update_set(&every_family_update()[4..5]).unwrap();

    let rejected = |updates: &[MarketParameterUpdate]| {
        assert_eq!(
            validate_parameter_update_set(updates).unwrap_err(),
            anchor_lang::prelude::error!(ErrorCode::ParameterUpdatesNotCanonical)
        );
    };
    rejected(&[]);

    let mut reversed = every_family_update();
    reversed.reverse();
    rejected(&reversed);

    let borrow_limit = |bps| MarketParameterUpdate::DailyBorrowLimit {
        max_daily_borrow_bps: bps,
    };
    rejected(&[borrow_limit(1_000), borrow_limit(1_000)]);
    rejected(&[borrow_limit(1_000), borrow_limit(2_000)]);

    let mut eight = every_family_update();
    eight.push(borrow_limit(1_000));
    rejected(&eight);
}

#[test]
fn only_updated_families_carry_a_revision() {
    let market_revisions = revisions([1, 2, 3, 4, 5, 6, 7]);
    let updates = vec![
        MarketParameterUpdate::Concentration {
            peak_amplification_nad: 1,
            core_half_width_bps: 1,
            fade_width_bps: 1,
        },
        MarketParameterUpdate::CenterController {
            adjustment_threshold_nad: 1,
            adjustment_step_nad: 1,
            min_adjustment_interval_slots: 1,
        },
    ];
    assert_eq!(
        updated_family_revisions(&updates, &market_revisions),
        revisions([0, 2, 0, 0, 0, 6, 0])
    );
}

#[test]
fn account_digest_rejects_any_post_creation_action_or_metadata_mutation() {
    let market = Pubkey::new_unique();
    let proposer = Pubkey::new_unique();
    let nonce = 42_u64;
    let (proposal_key, bump) = Pubkey::find_program_address(
        &[
            PARAMETER_PROPOSAL_SEED_PREFIX,
            market.as_ref(),
            proposer.as_ref(),
            &nonce.to_le_bytes(),
        ],
        &crate::ID,
    );
    let mut proposal = ParameterProposal {
        market: Pubkey::default(),
        proposer: Pubkey::default(),
        nonce: 0,
        updates: Vec::new(),
        family_revisions: [0; PARAMETER_FAMILY_COUNT],
        metadata: metadata(),
        digest: [0; 32],
        status: ParameterProposalStatus::Cancelled,
        sponsorship_floor: 0,
        total_locked: 0,
        queued_support: 0,
        queued_eligible_ylp: 0,
        created_at: 0,
        queued_at: 0,
        execute_after: 0,
        execution_deadline: 0,
        bump: 0,
    };
    proposal
        .initialize(
            market,
            proposer,
            nonce,
            &revisions([0, 0, 0, 0, 3, 0, 0]),
            vec![MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 1_000,
            }],
            metadata(),
            10_000,
            1,
            bump,
        )
        .unwrap();
    proposal.assert_account(market, proposal_key).unwrap();
    assert_eq!(proposal.family_revisions, revisions([0, 0, 0, 0, 3, 0, 0]));

    let created = proposal.updates.clone();
    proposal.updates[0] = MarketParameterUpdate::DailyBorrowLimit {
        max_daily_borrow_bps: 1_001,
    };
    assert_eq!(
        proposal.assert_account(market, proposal_key).unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidProposalDigest)
    );

    proposal.updates = created.clone();
    proposal.updates.push(MarketParameterUpdate::InsuranceDrawCaps {
        principal_coverage_bps: 100,
        per_day_bps: 200,
    });
    assert_eq!(
        proposal.assert_account(market, proposal_key).unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidProposalDigest)
    );

    proposal.updates = created;
    proposal.family_revisions[4] = 4;
    assert_eq!(
        proposal.assert_account(market, proposal_key).unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidProposalDigest)
    );

    proposal.family_revisions[4] = 3;
    proposal.updates.clear();
    assert_eq!(
        proposal.assert_account(market, proposal_key).unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidParameterProposal)
    );
}

#[test]
fn any_updated_family_changing_makes_the_proposal_stale() {
    let mut proposal = ParameterProposal {
        market: Pubkey::new_unique(),
        proposer: Pubkey::new_unique(),
        nonce: 1,
        updates: vec![
            MarketParameterUpdate::Concentration {
                peak_amplification_nad: 1,
                core_half_width_bps: 1,
                fade_width_bps: 1,
            },
            MarketParameterUpdate::CenterController {
                adjustment_threshold_nad: 1,
                adjustment_step_nad: 1,
                min_adjustment_interval_slots: 1,
            },
        ],
        family_revisions: revisions([0, 2, 0, 0, 0, 6, 0]),
        metadata: metadata(),
        digest: [0; 32],
        status: ParameterProposalStatus::Collecting,
        sponsorship_floor: 1,
        total_locked: 1,
        queued_support: 0,
        queued_eligible_ylp: 0,
        created_at: 1,
        queued_at: 0,
        execute_after: 0,
        execution_deadline: 0,
        bump: 1,
    };

    // Families the proposal does not touch may move freely.
    let untouched_moved = revisions([9, 2, 9, 9, 9, 6, 9]);
    assert!(proposal.revisions_current(&untouched_moved));
    assert!(!proposal.mark_stale_if_revision_changed(&untouched_moved));
    assert_eq!(proposal.status, ParameterProposalStatus::Collecting);

    let center_moved = revisions([0, 2, 0, 0, 0, 7, 0]);
    assert!(!proposal.revisions_current(&center_moved));
    assert!(proposal.mark_stale_if_revision_changed(&center_moved));
    assert_eq!(proposal.status, ParameterProposalStatus::Stale);
}

#[test]
fn sponsorship_rounds_up_and_queue_requires_more_than_half() {
    assert_eq!(sponsorship_floor(1).unwrap(), 1);
    assert_eq!(sponsorship_floor(10_001).unwrap(), 101);
    assert!(!has_strict_support_majority(500, 1_000).unwrap());
    assert!(has_strict_support_majority(501, 1_000).unwrap());
}

#[test]
fn virtual_yield_merges_whole_atoms_and_q64_remainders() {
    let mut ledger = VirtualYieldLedger::default();
    ledger.initialize(0, 0);
    ledger
        .accrue(3, YIELD_GROWTH_SCALE_Q64 / 2, YIELD_GROWTH_SCALE_Q64 / 2)
        .unwrap();
    assert_eq!(ledger.accrued_swap_fee_amount, 1);
    assert_eq!(ledger.swap_fee_remainder_q64, 1_u64 << 63);

    let mut yield_account = empty_yield_account();
    yield_account.swap_fee_remainder_q64 = 1_u64 << 63;
    ledger.merge_into(&mut yield_account).unwrap();
    assert_eq!(yield_account.accrued_swap_fee_amount, 2);
    assert_eq!(yield_account.swap_fee_remainder_q64, 0);
    assert_eq!(yield_account.accrued_interest_amount, 1);
    assert_eq!(yield_account.interest_remainder_q64, 1_u64 << 63);
}

#[test]
fn queue_snapshots_support_then_expiry_makes_support_unlockable() {
    let mut proposal = ParameterProposal {
        market: Pubkey::new_unique(),
        proposer: Pubkey::new_unique(),
        nonce: 1,
        updates: vec![MarketParameterUpdate::DailyBorrowLimit {
            max_daily_borrow_bps: 1_000,
        }],
        family_revisions: revisions([0, 0, 0, 0, 7, 0, 0]),
        metadata: metadata(),
        digest: [0; 32],
        status: ParameterProposalStatus::Collecting,
        sponsorship_floor: 1,
        total_locked: 51,
        queued_support: 0,
        queued_eligible_ylp: 0,
        created_at: 1,
        queued_at: 0,
        execute_after: 0,
        execution_deadline: 0,
        bump: 1,
    };
    assert!(proposal.queue_if_supported(100, 2).unwrap());
    assert_eq!(proposal.queued_support, 51);
    assert_eq!(proposal.queued_eligible_ylp, 100);
    assert!(proposal.mark_expired_if_past_deadline(proposal.execution_deadline + 1));
    assert_eq!(proposal.status, ParameterProposalStatus::Expired);
    assert!(!proposal.mark_stale_if_revision_changed(&revisions([0, 0, 0, 0, 8, 0, 0])));
}

#[test]
fn collecting_withdrawal_cancels_below_the_frozen_sponsorship_floor() {
    let mut proposal = ParameterProposal {
        market: Pubkey::new_unique(),
        proposer: Pubkey::new_unique(),
        nonce: 2,
        updates: vec![MarketParameterUpdate::Fee(FeeProfile::default())],
        family_revisions: [0; PARAMETER_FAMILY_COUNT],
        metadata: metadata(),
        digest: [0; 32],
        status: ParameterProposalStatus::Collecting,
        sponsorship_floor: 100,
        total_locked: 100,
        queued_support: 0,
        queued_eligible_ylp: 0,
        created_at: 1,
        queued_at: 0,
        execute_after: 0,
        execution_deadline: 0,
        bump: 1,
    };

    assert!(!proposal.cancel_if_below_sponsorship_floor());
    proposal.total_locked = 99;
    assert!(proposal.cancel_if_below_sponsorship_floor());
    assert_eq!(proposal.status, ParameterProposalStatus::Cancelled);
}

#[test]
fn governance_account_and_max_create_transaction_sizes_are_exact() {
    let max_metadata = ProposalMetadataV1 {
        version: PROPOSAL_METADATA_VERSION,
        title: "t".repeat(MAX_PROPOSAL_TITLE_BYTES),
        description_uri: "u".repeat(MAX_PROPOSAL_DESCRIPTION_URI_BYTES),
        description_sha256: [u8::MAX; 32],
        description_len: MAX_PROPOSAL_DESCRIPTION_BYTES,
    };
    // Every variant at its fixed Borsh size, so seven Fee updates bound the
    // account even though a valid proposal names each family at most once.
    let max_update = MarketParameterUpdate::Fee(FeeProfile::default());
    let proposal = ParameterProposal {
        market: Pubkey::new_unique(),
        proposer: Pubkey::new_unique(),
        nonce: u64::MAX,
        updates: vec![max_update; MAX_PARAMETER_UPDATES_PER_PROPOSAL],
        family_revisions: [u64::MAX; PARAMETER_FAMILY_COUNT],
        metadata: max_metadata.clone(),
        digest: [u8::MAX; 32],
        status: ParameterProposalStatus::Queued,
        sponsorship_floor: u64::MAX,
        total_locked: u64::MAX,
        queued_support: u64::MAX,
        queued_eligible_ylp: u64::MAX,
        created_at: i64::MAX,
        queued_at: i64::MAX,
        execute_after: i64::MAX,
        execution_deadline: i64::MAX,
        bump: u8::MAX,
    };
    let support = ProposalSupport {
        proposal: Pubkey::new_unique(),
        supporter: Pubkey::new_unique(),
        locked_amount: u64::MAX,
        base_yield: VirtualYieldLedger {
            swap_fee_checkpoint_q64: u128::MAX,
            interest_checkpoint_q64: u128::MAX,
            accrued_swap_fee_amount: u64::MAX,
            accrued_interest_amount: u64::MAX,
            swap_fee_remainder_q64: u64::MAX,
            interest_remainder_q64: u64::MAX,
        },
        quote_yield: VirtualYieldLedger {
            swap_fee_checkpoint_q64: u128::MAX,
            interest_checkpoint_q64: u128::MAX,
            accrued_swap_fee_amount: u64::MAX,
            accrued_interest_amount: u64::MAX,
            swap_fee_remainder_q64: u64::MAX,
            interest_remainder_q64: u64::MAX,
        },
        bump: u8::MAX,
    };

    assert_eq!(proposal.try_to_vec().unwrap().len(), ParameterProposal::INIT_SPACE);
    assert_eq!(ParameterProposal::INIT_SPACE, 1_187);
    // A finished proposal keeps only its tombstone discriminator.
    assert_eq!(
        crate::account::get_size_with_discriminator::<crate::state::ParameterProposalTombstone>(),
        8
    );
    assert_eq!(support.try_to_vec().unwrap().len(), ProposalSupport::INIT_SPACE);
    assert_eq!(ProposalSupport::INIT_SPACE, 201);

    let proposer = proposal.proposer;
    let args = CreateParameterProposalArgs {
        nonce: proposal.nonce,
        updates: every_family_update(),
        metadata: max_metadata,
        initial_support: u64::MAX,
    };
    let instruction_data = crate::instruction::CreateParameterProposal { args }.data();
    assert_eq!(instruction_data.len(), 555);

    let account_metas = crate::accounts::CreateParameterProposal {
        proposer,
        market: proposal.market,
        proposal: Pubkey::new_unique(),
        proposal_support: support.proposal,
        ylp_mint: Pubkey::new_unique(),
        proposer_ylp_account: Pubkey::new_unique(),
        base_yield_account: Pubkey::new_unique(),
        quote_yield_account: Pubkey::new_unique(),
        base_hlp_ylp_vault: Pubkey::new_unique(),
        quote_hlp_ylp_vault: Pubkey::new_unique(),
        token_2022_program: anchor_spl::token_2022::ID,
        system_program: anchor_lang::system_program::ID,
        event_authority: Pubkey::find_program_address(&[b"__event_authority"], &crate::ID).0,
        program: crate::ID,
    }
    .to_account_metas(None);
    assert_eq!(account_metas.len(), 14);

    let message = anchor_lang::solana_program::message::Message::new(
        &[anchor_lang::solana_program::instruction::Instruction {
            program_id: crate::ID,
            accounts: account_metas,
            data: instruction_data,
        }],
        Some(&proposer),
    );
    assert_eq!(message.header.num_required_signatures, 1);
    assert_eq!(message.account_keys.len(), 14);
    let message_size = bincode::serialize(&message).unwrap().len();
    assert_eq!(message_size, 1_058);
    // One compact-u16 signature count byte, one signature, and the message.
    assert_eq!(1 + 64 + message_size, 1_123);
    // A proposal that changes every family still fits one 1,232-byte packet.
    assert!(1 + 64 + message_size <= 1_232);
}

#[test]
fn lifecycle_boundaries_keep_support_frozen_through_the_deadline() {
    let mut proposal = ParameterProposal {
        market: Pubkey::new_unique(),
        proposer: Pubkey::new_unique(),
        nonce: 3,
        updates: vec![MarketParameterUpdate::Irm(IrmConfig::default())],
        family_revisions: revisions([0, 0, 5, 0, 0, 0, 0]),
        metadata: metadata(),
        digest: [0; 32],
        status: ParameterProposalStatus::Collecting,
        sponsorship_floor: 1,
        total_locked: 51,
        queued_support: 0,
        queued_eligible_ylp: 0,
        created_at: 1,
        queued_at: 0,
        execute_after: 0,
        execution_deadline: 0,
        bump: 1,
    };

    assert!(proposal.queue_if_supported(100, 10).unwrap());
    assert_eq!(proposal.execute_after, 10 + PARAMETER_PROPOSAL_TIMELOCK_SECONDS);
    assert_eq!(
        proposal.execution_deadline,
        proposal.execute_after + PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS
    );
    assert!(!proposal.mark_expired_if_past_deadline(proposal.execution_deadline));
    assert_eq!(proposal.status, ParameterProposalStatus::Queued);
    assert!(proposal.mark_expired_if_past_deadline(proposal.execution_deadline + 1));
    assert_eq!(proposal.status, ParameterProposalStatus::Expired);
}
