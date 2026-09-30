use crate::{mock::*, Error, Escrows, Event, HoldReason, NextEscrowId};
use frame_support::{
	assert_noop, assert_ok,
	traits::{fungible::InspectHold, ConstU32},
	BoundedVec,
};

type Milestones = BoundedVec<u64, ConstU32<MAX_MILESTONES>>;

fn ms(amounts: &[u64]) -> Milestones {
	amounts.to_vec().try_into().expect("test milestones fit the bound")
}

fn held(who: u64) -> u64 {
	Balances::balance_on_hold(&HoldReason::Escrow.into(), &who)
}

fn deposit_held(who: u64) -> u64 {
	Balances::balance_on_hold(&HoldReason::StorageDeposit.into(), &who)
}

fn open(milestones: &[u64], arbiter: Option<u64>, deadline: u64) -> u32 {
	let id = NextEscrowId::<Test>::get();
	assert_ok!(Escrow::create(
		RuntimeOrigin::signed(PAYER),
		BENEFICIARY,
		arbiter,
		ms(milestones),
		deadline
	));
	id
}

/// Run a test body and check the pallet's accounting invariant afterwards.
fn run(test: impl FnOnce()) {
	new_test_ext().execute_with(|| {
		test();
		Escrow::do_try_state().expect("try_state invariant holds");
	});
}

#[test]
fn create_holds_total_and_deposit() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_eq!(held(PAYER), 150);
		assert_eq!(deposit_held(PAYER), DEPOSIT);
		assert_eq!(Balances::free_balance(PAYER), START_BALANCE - 150 - DEPOSIT);
		let escrow = Escrows::<Test>::get(id).unwrap();
		assert_eq!(escrow.remaining, 150);
		assert_eq!(escrow.released, 0);
		System::assert_last_event(
			Event::Created { id, payer: PAYER, beneficiary: BENEFICIARY, total: 150 }.into(),
		);
	});
}

#[test]
fn releases_pay_in_order_and_completion_cleans_up() {
	run(|| {
		let id = open(&[100, 50], None, 10);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), id));
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 100);
		assert_eq!(held(PAYER), 50);

		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), id));
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 150);
		assert_eq!(held(PAYER), 0);
		assert_eq!(deposit_held(PAYER), 0);
		assert_eq!(Balances::free_balance(PAYER), START_BALANCE - 150);
		assert!(Escrows::<Test>::get(id).is_none());
		System::assert_last_event(Event::Completed { id }.into());
	});
}

#[test]
fn arbiter_can_release() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(ARBITER), id));
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 100);
	});
}

// ESC-04: v0 let any signed account trigger a payout.
#[test]
fn stranger_and_beneficiary_cannot_release() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		assert_noop!(
			Escrow::release(RuntimeOrigin::signed(STRANGER), id),
			Error::<Test>::NotAuthorized
		);
		assert_noop!(
			Escrow::release(RuntimeOrigin::signed(BENEFICIARY), id),
			Error::<Test>::NotAuthorized
		);
	});
}

// ESC-01: v0 paid an arbiter refund to the arbiter.
#[test]
fn arbiter_refund_pays_the_payer_not_the_arbiter() {
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		assert_ok!(Escrow::refund(RuntimeOrigin::signed(ARBITER), id));
		assert_eq!(Balances::free_balance(ARBITER), START_BALANCE);
		assert_eq!(Balances::free_balance(PAYER), START_BALANCE);
		assert_eq!(held(PAYER), 0);
		System::assert_last_event(Event::Refunded { id, amount: 150 }.into());
	});
}

// ESC-05: v0 let the payer take the money back before the deadline.
#[test]
fn payer_refund_waits_for_deadline() {
	run(|| {
		let id = open(&[100], None, 10);
		assert_noop!(
			Escrow::refund(RuntimeOrigin::signed(PAYER), id),
			Error::<Test>::DeadlineNotReached
		);
		System::set_block_number(10);
		assert_ok!(Escrow::refund(RuntimeOrigin::signed(PAYER), id));
		assert_eq!(Balances::free_balance(PAYER), START_BALANCE);
	});
}

#[test]
fn refund_after_partial_release_returns_only_the_rest() {
	run(|| {
		let id = open(&[100, 50], None, 10);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), id));
		System::set_block_number(10);
		assert_ok!(Escrow::refund(RuntimeOrigin::signed(PAYER), id));
		assert_eq!(Balances::free_balance(PAYER), START_BALANCE - 100);
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 100);
	});
}

#[test]
fn only_parties_can_refund() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		System::set_block_number(20);
		for who in [BENEFICIARY, STRANGER] {
			assert_noop!(
				Escrow::refund(RuntimeOrigin::signed(who), id),
				Error::<Test>::NotAuthorized
			);
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
		assert_ok!(Escrow::cancel(RuntimeOrigin::signed(BENEFICIARY), first));
		assert_noop!(
			Escrow::cancel(RuntimeOrigin::signed(BENEFICIARY), first),
			Error::<Test>::NotFound
		);
		assert_eq!(held(PAYER), 200);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), second));
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 200);
	});
}

#[test]
fn only_the_beneficiary_can_cancel() {
	run(|| {
		let id = open(&[100], Some(ARBITER), 10);
		for who in [PAYER, ARBITER, STRANGER] {
			assert_noop!(
				Escrow::cancel(RuntimeOrigin::signed(who), id),
				Error::<Test>::NotAuthorized
			);
		}
	});
}

// ESC-03: v0 summed milestones with `+`, which wraps in a release build.
#[test]
fn milestone_overflow_is_rejected() {
	run(|| {
		assert_noop!(
			Escrow::create(
				RuntimeOrigin::signed(PAYER),
				BENEFICIARY,
				None,
				ms(&[u64::MAX, MIN_MILESTONE]),
				10
			),
			Error::<Test>::Overflow
		);
	});
}

// ESC-06: v0 panicked on an unknown id and on a release after completion.
#[test]
fn unknown_or_finished_escrow_is_an_error_not_a_panic() {
	run(|| {
		assert_noop!(Escrow::release(RuntimeOrigin::signed(PAYER), 42), Error::<Test>::NotFound);
		assert_noop!(Escrow::refund(RuntimeOrigin::signed(PAYER), 42), Error::<Test>::NotFound);
		assert_noop!(
			Escrow::cancel(RuntimeOrigin::signed(BENEFICIARY), 42),
			Error::<Test>::NotFound
		);
		let id = open(&[100], None, 10);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), id));
		assert_noop!(Escrow::release(RuntimeOrigin::signed(PAYER), id), Error::<Test>::NotFound);
	});
}

// ESC-08: input validation.
#[test]
fn create_rejects_bad_input() {
	run(|| {
		let create = |beneficiary, arbiter, milestones: &[u64], deadline| {
			Escrow::create(
				RuntimeOrigin::signed(PAYER),
				beneficiary,
				arbiter,
				ms(milestones),
				deadline,
			)
		};
		assert_noop!(create(PAYER, None, &[100], 10), Error::<Test>::SelfEscrow);
		assert_noop!(create(BENEFICIARY, Some(PAYER), &[100], 10), Error::<Test>::ArbiterConflict);
		assert_noop!(
			create(BENEFICIARY, Some(BENEFICIARY), &[100], 10),
			Error::<Test>::ArbiterConflict
		);
		assert_noop!(create(BENEFICIARY, None, &[], 10), Error::<Test>::NoMilestones);
		assert_noop!(
			create(BENEFICIARY, None, &[100, MIN_MILESTONE - 1], 10),
			Error::<Test>::MilestoneTooSmall
		);
		assert_noop!(create(BENEFICIARY, None, &[100], 1), Error::<Test>::DeadlineInPast);
	});
}

#[test]
fn create_without_funds_changes_nothing() {
	run(|| {
		// Enough for the milestones but not the deposit on top: the first hold succeeds, the
		// second fails, and the transactional dispatch must roll back both.
		assert!(Escrow::create(
			RuntimeOrigin::signed(PAYER),
			BENEFICIARY,
			None,
			ms(&[START_BALANCE - 1]),
			10
		)
		.is_err());
		assert_eq!(held(PAYER), 0);
		assert_eq!(deposit_held(PAYER), 0);
		assert_eq!(NextEscrowId::<Test>::get(), 0);
	});
}

#[test]
fn escrows_from_one_payer_are_accounted_separately() {
	run(|| {
		let a = open(&[100, 100], Some(ARBITER), 10);
		let b = open(&[300], None, 10);
		let c = open(&[50, 50, 50], None, 10);
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), a));
		assert_ok!(Escrow::cancel(RuntimeOrigin::signed(BENEFICIARY), b));
		assert_ok!(Escrow::refund(RuntimeOrigin::signed(ARBITER), a));
		assert_eq!(held(PAYER), 150);
		assert_eq!(deposit_held(PAYER), DEPOSIT);
		assert!(Escrows::<Test>::get(c).is_some());
	});
}

// ESC-11: a lock applies to the whole balance, held funds included. Transferring out of the hold
// politely let a payer who locked their balance (a conviction vote, say) block every payout,
// the arbiter's included, then refund after the deadline.
#[test]
#[allow(deprecated)]
fn payer_lock_cannot_block_a_payout() {
	use frame_support::traits::{LockableCurrency, WithdrawReasons};
	run(|| {
		let id = open(&[100, 50], Some(ARBITER), 10);
		Balances::set_lock(*b"democrac", &PAYER, START_BALANCE, WithdrawReasons::all());
		assert_ok!(Escrow::release(RuntimeOrigin::signed(ARBITER), id));
		assert_ok!(Escrow::release(RuntimeOrigin::signed(PAYER), id));
		assert_eq!(Balances::free_balance(BENEFICIARY), START_BALANCE + 150);
	});
}

#[test]
fn integrity_test_passes_for_mock_config() {
	use frame_support::traits::Hooks;
	crate::Pallet::<Test>::integrity_test();
}
