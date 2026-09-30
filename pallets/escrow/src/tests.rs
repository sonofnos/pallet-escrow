use crate::{mock::*, Claim, Error, Escrows, Event, Milestone, NextEscrowId};
use frame_support::{
	assert_noop, assert_ok,
	traits::{fungible::Mutate, tokens::Preservation, ConstU32},
	BoundedVec,
};

type Milestones = BoundedVec<Milestone<u64, u64>, ConstU32<MAX_MILESTONES>>;

/// Milestones as `(amount, deadline)`.
fn ms(milestones: &[(u64, u64)]) -> Milestones {
	milestones
		.iter()
		.map(|&(amount, deadline)| Milestone { amount, deadline })
		.collect::<Vec<_>>()
		.try_into()
		.expect("test milestones fit the bound")
}

/// Milestones sharing one deadline.
fn due(amounts: &[u64], deadline: u64) -> Milestones {
	ms(&amounts.iter().map(|a| (*a, deadline)).collect::<Vec<_>>())
}

fn free(who: AccountId) -> u64 {
	Balances::free_balance(who)
}

fn in_escrow(id: u32) -> u64 {
	Balances::free_balance(Escrow::account(id))
}

fn open_with(milestones: Milestones, arbiter: Option<AccountId>) -> u32 {
	let id = NextEscrowId::<Test>::get();
	assert_ok!(Escrow::create(RuntimeOrigin::signed(PAYER), BENEFICIARY, arbiter, milestones));
	id
}

fn open(amounts: &[u64], arbiter: Option<AccountId>, deadline: u64) -> u32 {
	open_with(due(amounts, deadline), arbiter)
}

fn signed(who: AccountId) -> RuntimeOrigin {
	RuntimeOrigin::signed(who)
}

/// Run a test body and check the pallet's custody invariant afterwards.
fn run(test: impl FnOnce()) {
	new_test_ext().execute_with(|| {
		test();
		Escrow::do_try_state().expect("try_state invariant holds");
	});
}

#[test]
fn create_moves_total_and_deposit_into_the_escrow_account() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_eq!(in_escrow(id), 150 + DEPOSIT);
		assert_eq!(free(PAYER), START_BALANCE - 150 - DEPOSIT);
		let escrow = Escrows::<Test>::get(id).unwrap();
		assert_eq!(escrow.remaining, 150);
		assert_eq!(escrow.deposit, DEPOSIT);
		System::assert_last_event(
			Event::Created { id, payer: PAYER, beneficiary: BENEFICIARY, total: 150 }.into(),
		);
	});
}

#[test]
fn releases_pay_in_order_and_completion_cleans_up() {
	run(|| {
		let id = open(&[100, 50], None, 10);
		assert_ok!(Escrow::release(signed(PAYER), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 100);
		assert_eq!(in_escrow(id), 50 + DEPOSIT);

		assert_ok!(Escrow::release(signed(PAYER), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 150);
		assert_eq!(free(PAYER), START_BALANCE - 150);
		assert!(Escrows::<Test>::get(id).is_none());
		assert!(!System::account_exists(&Escrow::account(id)));
		System::assert_last_event(Event::Completed { id }.into());
	});
}

#[test]
fn arbiter_can_release() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		assert_ok!(Escrow::release(signed(ARBITER), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 100);
	});
}

// ESC-04: v0 let any signed account trigger a payout.
#[test]
fn stranger_and_beneficiary_cannot_release() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		for who in [STRANGER, BENEFICIARY] {
			assert_noop!(Escrow::release(signed(who), id), Error::<Test>::NotAuthorized);
		}
	});
}

// ESC-01: v0 paid an arbiter refund to the arbiter.
#[test]
fn arbiter_refund_pays_the_payer_not_the_arbiter() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_ok!(Escrow::refund(signed(ARBITER), id));
		assert_eq!(free(ARBITER), START_BALANCE);
		assert_eq!(free(PAYER), START_BALANCE);
		System::assert_last_event(Event::Refunded { id, amount: 150 }.into());
	});
}

// ESC-05: v0 let the payer take the money back before the deadline.
#[test]
fn payer_refund_waits_for_the_next_deadline() {
	run(|| {
		let id = open_with(ms(&[(100, 10), (50, 20)]), None);
		assert_noop!(Escrow::refund(signed(PAYER), id), Error::<Test>::DeadlineNotReached);
		assert_ok!(Escrow::release(signed(PAYER), id));
		System::set_block_number(15);
		assert_noop!(Escrow::refund(signed(PAYER), id), Error::<Test>::DeadlineNotReached);
		System::set_block_number(20);
		assert_ok!(Escrow::refund(signed(PAYER), id));
		assert_eq!(free(PAYER), START_BALANCE - 100);
		assert_eq!(free(BENEFICIARY), START_BALANCE + 100);
	});
}

#[test]
fn only_parties_can_refund() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		System::set_block_number(20);
		for who in [BENEFICIARY, STRANGER] {
			assert_noop!(Escrow::refund(signed(who), id), Error::<Test>::NotAuthorized);
		}
	});
}

// ESC-02: v0 left a cancelled escrow in storage, so each repeat cancel released more of the
// payer's shared hold, unfunding their other escrows.
#[test]
fn cancel_is_final_and_leaves_other_escrows_funded() {
	run(|| {
		let first = open(&[100], None, 10);
		let second = open(&[200], None, 10);
		assert_ok!(Escrow::cancel(signed(BENEFICIARY), first));
		assert_noop!(Escrow::cancel(signed(BENEFICIARY), first), Error::<Test>::NotFound);
		assert_eq!(in_escrow(second), 200 + DEPOSIT);
		assert_ok!(Escrow::release(signed(PAYER), second));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 200);
	});
}

#[test]
fn only_the_beneficiary_can_cancel() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		for who in [PAYER, ARBITER, STRANGER] {
			assert_noop!(Escrow::cancel(signed(who), id), Error::<Test>::NotAuthorized);
		}
	});
}

// ESC-03: v0 summed milestones with `+`, which wraps in a release build.
#[test]
fn milestone_overflow_is_rejected() {
	run(|| {
		let create =
			|amounts: &[u64]| Escrow::create(signed(PAYER), BENEFICIARY, None, due(amounts, 10));
		assert_noop!(create(&[u64::MAX, MIN_MILESTONE]), Error::<Test>::Overflow);
		// The total fits, the total plus the deposit does not.
		assert_noop!(create(&[u64::MAX - 1]), Error::<Test>::Overflow);
	});
}

// ESC-06: v0 panicked on an unknown id and on a release after completion.
#[test]
fn unknown_or_finished_escrow_is_an_error_not_a_panic() {
	run(|| {
		assert_noop!(Escrow::release(signed(PAYER), 42), Error::<Test>::NotFound);
		assert_noop!(Escrow::refund(signed(PAYER), 42), Error::<Test>::NotFound);
		assert_noop!(Escrow::cancel(signed(BENEFICIARY), 42), Error::<Test>::NotFound);
		assert_noop!(Escrow::submit(signed(BENEFICIARY), 42), Error::<Test>::NotFound);
		let id = open(&[100], None, 10);
		assert_ok!(Escrow::release(signed(PAYER), id));
		assert_noop!(Escrow::release(signed(PAYER), id), Error::<Test>::NotFound);
	});
}

// ESC-08: input validation.
#[test]
fn create_rejects_bad_input() {
	run(|| {
		let create = |beneficiary, arbiter, milestones| {
			Escrow::create(signed(PAYER), beneficiary, arbiter, milestones)
		};
		assert_noop!(create(PAYER, None, due(&[100], 10)), Error::<Test>::SelfEscrow);
		assert_noop!(
			create(BENEFICIARY, Some(PAYER), due(&[100], 10)),
			Error::<Test>::ArbiterConflict
		);
		assert_noop!(
			create(BENEFICIARY, Some(BENEFICIARY), due(&[100], 10)),
			Error::<Test>::ArbiterConflict
		);
		assert_noop!(create(BENEFICIARY, None, due(&[], 10)), Error::<Test>::NoMilestones);
		assert_noop!(
			create(BENEFICIARY, None, due(&[100, MIN_MILESTONE - 1], 10)),
			Error::<Test>::MilestoneTooSmall
		);
		assert_noop!(create(BENEFICIARY, None, due(&[100], 1)), Error::<Test>::DeadlineInPast);
		assert_noop!(
			create(BENEFICIARY, None, ms(&[(100, 20), (100, 10)])),
			Error::<Test>::DeadlinesOutOfOrder
		);
	});
}

#[test]
fn create_without_funds_changes_nothing() {
	run(|| {
		// Enough for the milestones but not the deposit on top.
		assert!(Escrow::create(signed(PAYER), BENEFICIARY, None, due(&[START_BALANCE - 1], 10))
			.is_err());
		assert_eq!(free(PAYER), START_BALANCE);
		assert_eq!(NextEscrowId::<Test>::get(), 0);
	});
}

#[test]
fn escrows_are_held_in_separate_accounts() {
	run(|| {
		let a = open(&[100, 100], Some(ARBITER), 10);
		let b = open(&[300], None, 10);
		let c = open(&[50, 50, 50], None, 10);
		assert_ne!(Escrow::account(a), Escrow::account(b));
		assert_ok!(Escrow::release(signed(PAYER), a));
		assert_ok!(Escrow::cancel(signed(BENEFICIARY), b));
		assert_ok!(Escrow::refund(signed(ARBITER), a));
		assert_eq!(in_escrow(a), 0);
		assert_eq!(in_escrow(b), 0);
		assert_eq!(in_escrow(c), 150 + DEPOSIT);
	});
}

// ESC-11: in the hold-based pallet a lock on the payer covered the escrowed funds, so a payer who
// locked their balance could block every payout. Here the funds have left the payer's account.
#[test]
#[allow(deprecated)]
fn payer_lock_cannot_block_a_payout() {
	use frame_support::traits::{LockableCurrency, WithdrawReasons};
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		Balances::set_lock(*b"democrac", &PAYER, START_BALANCE, WithdrawReasons::all());
		assert_ok!(Escrow::release(signed(ARBITER), id));
		assert_ok!(Escrow::release(signed(PAYER), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 150);
	});
}

#[test]
#[allow(deprecated)]
fn locked_funds_cannot_be_escrowed() {
	use frame_support::traits::{LockableCurrency, WithdrawReasons};
	run(|| {
		Balances::set_lock(*b"democrac", &PAYER, START_BALANCE - 100, WithdrawReasons::all());
		assert!(Escrow::create(signed(PAYER), BENEFICIARY, None, due(&[200], 10)).is_err());
		open(&[90], None, 10);
	});
}

#[test]
fn funds_sent_to_an_escrow_account_go_to_the_payer_on_close() {
	run(|| {
		let id = open(&[100], None, 10);
		assert_ok!(<Balances as Mutate<_>>::transfer(
			&STRANGER,
			&Escrow::account(id),
			7,
			Preservation::Expendable
		));
		assert_ok!(Escrow::cancel(signed(BENEFICIARY), id));
		assert_eq!(free(PAYER), START_BALANCE + 7);
		assert!(!System::account_exists(&Escrow::account(id)));
	});
}

#[test]
fn submit_needs_an_arbiter_and_the_beneficiary() {
	run(|| {
		let alone = open(&[100], None, 10);
		assert_noop!(Escrow::submit(signed(BENEFICIARY), alone), Error::<Test>::NoArbiter);
		let id = open(&[100], Some(ARBITER), 10);
		for who in [PAYER, ARBITER, STRANGER] {
			assert_noop!(Escrow::submit(signed(who), id), Error::<Test>::NotAuthorized);
		}
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_noop!(Escrow::submit(signed(BENEFICIARY), id), Error::<Test>::ClaimPending);
		System::assert_last_event(Event::Submitted { id, index: 0 }.into());
	});
}

#[test]
fn submit_closes_at_the_deadline() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		System::set_block_number(10);
		assert_noop!(Escrow::submit(signed(BENEFICIARY), id), Error::<Test>::DeadlinePassed);
	});
}

#[test]
fn undisputed_claim_pays_after_the_challenge_period() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_noop!(Escrow::claim(signed(BENEFICIARY), id), Error::<Test>::ChallengeNotOver);
		System::set_block_number(1 + CHALLENGE_PERIOD);
		assert_noop!(Escrow::claim(signed(PAYER), id), Error::<Test>::NotAuthorized);
		assert_ok!(Escrow::claim(signed(BENEFICIARY), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 100);
		assert_eq!(Escrows::<Test>::get(id).unwrap().claim, None);
		assert_noop!(Escrow::claim(signed(BENEFICIARY), id), Error::<Test>::NoClaim);
	});
}

// The flaw the claim flow exists for: a payer who stalls until the deadline and refunds.
#[test]
fn a_pending_claim_blocks_the_payers_refund() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		System::set_block_number(9);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		System::set_block_number(10);
		assert_noop!(Escrow::refund(signed(PAYER), id), Error::<Test>::ClaimPending);
		System::set_block_number(9 + CHALLENGE_PERIOD);
		assert_ok!(Escrow::claim(signed(BENEFICIARY), id));
		assert_eq!(free(BENEFICIARY), START_BALANCE + 100);
	});
}

#[test]
fn dispute_only_by_the_payer_within_the_challenge_period() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		assert_noop!(Escrow::dispute(signed(PAYER), id), Error::<Test>::NoClaim);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		for who in [BENEFICIARY, ARBITER, STRANGER] {
			assert_noop!(Escrow::dispute(signed(who), id), Error::<Test>::NotAuthorized);
		}
		System::set_block_number(1 + CHALLENGE_PERIOD);
		assert_noop!(Escrow::dispute(signed(PAYER), id), Error::<Test>::ChallengeOver);
	});
}

#[test]
fn a_disputed_claim_waits_for_the_arbiter() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_ok!(Escrow::dispute(signed(PAYER), id));
		assert_noop!(Escrow::dispute(signed(PAYER), id), Error::<Test>::AlreadyDisputed);
		System::set_block_number(20);
		assert_noop!(Escrow::claim(signed(BENEFICIARY), id), Error::<Test>::Disputed);
		assert_noop!(Escrow::refund(signed(PAYER), id), Error::<Test>::ClaimPending);
		assert_eq!(
			Escrows::<Test>::get(id).unwrap().claim,
			Some(Claim { submitted: 1, disputed: true })
		);
	});
}

#[test]
fn resolve_splits_the_milestone() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_noop!(Escrow::resolve(signed(ARBITER), id, 60), Error::<Test>::NotDisputed);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_noop!(Escrow::resolve(signed(ARBITER), id, 60), Error::<Test>::NotDisputed);
		assert_ok!(Escrow::dispute(signed(PAYER), id));
		assert_noop!(Escrow::resolve(signed(PAYER), id, 60), Error::<Test>::NotAuthorized);

		assert_ok!(Escrow::resolve(signed(ARBITER), id, 60));
		System::assert_last_event(
			Event::Resolved { id, index: 0, to_beneficiary: 60, to_payer: 40 }.into(),
		);
		assert_eq!(free(BENEFICIARY), START_BALANCE + 60);
		assert_eq!(free(PAYER), START_BALANCE - 150 - DEPOSIT + 40);
		let escrow = Escrows::<Test>::get(id).unwrap();
		assert_eq!((escrow.released, escrow.remaining, escrow.claim), (1, 50, None));
	});
}

#[test]
fn resolve_rejects_shares_below_the_minimum() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_ok!(Escrow::dispute(signed(PAYER), id));
		for share in [101, MIN_MILESTONE - 1, 100 - MIN_MILESTONE + 1] {
			assert_noop!(Escrow::resolve(signed(ARBITER), id, share), Error::<Test>::InvalidSplit);
		}
		// All to one side is always a valid split, and the last milestone closes the escrow.
		assert_ok!(Escrow::resolve(signed(ARBITER), id, 0));
		assert_eq!(free(PAYER), START_BALANCE);
		assert!(Escrows::<Test>::get(id).is_none());
	});
}

#[test]
fn release_settles_a_pending_claim() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		assert_ok!(Escrow::dispute(signed(PAYER), id));
		assert_ok!(Escrow::release(signed(PAYER), id));
		assert_eq!(Escrows::<Test>::get(id).unwrap().claim, None);
		assert_ok!(Escrow::submit(signed(BENEFICIARY), id));
		System::assert_last_event(Event::Submitted { id, index: 1 }.into());
	});
}

#[test]
fn integrity_test_passes_for_mock_config() {
	use frame_support::traits::Hooks;
	crate::Pallet::<Test>::integrity_test();
}

mod migration {
	use super::*;
	use crate::{
		migrations::v1::{self, OldEscrowInfo},
		HoldReason,
	};
	use frame_support::traits::{
		fungible::{InspectHold, MutateHold},
		GetStorageVersion, OnRuntimeUpgrade, StorageVersion,
	};

	/// Write an escrow the way the hold-based pallet stored it.
	fn old_escrow(id: u32, milestones: &[u64], released: u32, deposit: u64) {
		let remaining: u64 = milestones[released as usize..].iter().sum();
		assert_ok!(Balances::hold(&HoldReason::Escrow.into(), &PAYER, remaining));
		assert_ok!(Balances::hold(&HoldReason::StorageDeposit.into(), &PAYER, deposit));
		v1::Escrows::<Test>::insert(
			id,
			OldEscrowInfo::<Test> {
				payer: PAYER,
				beneficiary: BENEFICIARY,
				arbiter: Some(ARBITER),
				milestones: milestones.to_vec().try_into().unwrap(),
				released,
				remaining,
				deposit,
				deadline: 30,
			},
		);
		v1::EscrowCount::<Test>::mutate(PAYER, |n| *n += 1);
	}

	#[test]
	fn moves_holds_into_escrow_accounts() {
		new_test_ext().execute_with(|| {
			StorageVersion::new(0).put::<crate::Pallet<Test>>();
			old_escrow(0, &[100, 50], 1, DEPOSIT);
			// Version 0 allowed a deposit below the existential deposit.
			old_escrow(1, &[200], 0, 0);
			NextEscrowId::<Test>::put(2);

			v1::MigrateToV1::<Test>::on_runtime_upgrade();

			assert_eq!(crate::Pallet::<Test>::on_chain_storage_version(), 1);
			for reason in [HoldReason::Escrow, HoldReason::StorageDeposit] {
				assert_eq!(Balances::balance_on_hold(&reason.into(), &PAYER), 0);
			}
			assert!(v1::EscrowCount::<Test>::iter().next().is_none());
			assert_eq!(in_escrow(0), 50 + DEPOSIT);
			let ed = <Balances as frame_support::traits::fungible::Inspect<_>>::minimum_balance();
			assert_eq!(in_escrow(1), 200 + ed);
			let first = Escrows::<Test>::get(0).unwrap();
			assert_eq!(first.milestones, ms(&[(100, 30), (50, 30)]));
			assert_eq!((first.released, first.remaining, first.claim), (1, 50, None));
			Escrow::do_try_state().unwrap();

			// Migrated escrows behave like new ones.
			assert_ok!(Escrow::release(signed(PAYER), 0));
			assert_ok!(Escrow::cancel(signed(BENEFICIARY), 1));
			// Only the second milestone of escrow 0 was still owed; everything else came back.
			assert_eq!(free(PAYER), START_BALANCE - 50);
		});
	}

	#[test]
	fn runs_once() {
		new_test_ext().execute_with(|| {
			assert_eq!(crate::Pallet::<Test>::on_chain_storage_version(), 1);
			let id = open(&[100], None, 10);
			v1::MigrateToV1::<Test>::on_runtime_upgrade();
			assert!(Escrows::<Test>::get(id).is_some());
		});
	}
}
