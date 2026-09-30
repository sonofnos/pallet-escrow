//! Benchmarks for `pallet_escrow`. Every call is measured at its worst case.

use super::*;
use crate::pallet::{BalanceOf, EscrowId, Escrows, Milestone, MilestoneOf, NextEscrowId};
use frame_benchmarking::v2::*;
use frame_support::{
	traits::{fungible::Mutate, Get},
	BoundedVec,
};
use frame_system::{pallet_prelude::BlockNumberFor, RawOrigin};
use sp_runtime::traits::{Bounded, Saturating};

fn funded<T: Config>(name: &'static str, index: u32) -> T::AccountId {
	let who: T::AccountId = account(name, index, 0);
	// An eighth of the maximum each, so funding every party cannot overflow total issuance.
	let amount = BalanceOf::<T>::max_value() / 8u32.into();
	T::Currency::set_balance(&who, amount);
	who
}

fn now<T: Config>() -> BlockNumberFor<T> {
	frame_system::Pallet::<T>::block_number()
}

fn milestones<T: Config>(count: u32) -> BoundedVec<MilestoneOf<T>, T::MaxMilestones> {
	let amount = T::MinMilestone::get().saturating_mul(10u32.into());
	let deadline = now::<T>().saturating_add(10u32.into());
	let mut out = BoundedVec::new();
	for _ in 0..count {
		out.try_push(Milestone { amount, deadline }).expect("count is at most MaxMilestones");
	}
	out
}

/// Open an escrow with two milestones and return its id plus the parties.
fn open<T: Config>() -> (EscrowId, T::AccountId, T::AccountId, T::AccountId) {
	let payer = funded::<T>("payer", 0);
	let beneficiary = funded::<T>("beneficiary", 0);
	let arbiter = funded::<T>("arbiter", 0);
	let id = NextEscrowId::<T>::get();
	Pallet::<T>::create(
		RawOrigin::Signed(payer.clone()).into(),
		beneficiary.clone(),
		Some(arbiter.clone()),
		milestones::<T>(2),
	)
	.expect("benchmark escrow opens");
	(id, payer, beneficiary, arbiter)
}

/// Open an escrow and bring it to its last milestone, submitted by the beneficiary.
fn last_submitted<T: Config>() -> (EscrowId, T::AccountId, T::AccountId, T::AccountId) {
	let (id, payer, beneficiary, arbiter) = open::<T>();
	Pallet::<T>::release(RawOrigin::Signed(payer.clone()).into(), id)
		.expect("first milestone releases");
	Pallet::<T>::submit(RawOrigin::Signed(beneficiary.clone()).into(), id)
		.expect("last milestone submits");
	(id, payer, beneficiary, arbiter)
}

#[benchmarks]
mod benchmarks {
	use super::*;

	#[benchmark]
	fn create(m: Linear<1, { T::MaxMilestones::get() }>) {
		let payer = funded::<T>("payer", 0);
		let beneficiary = funded::<T>("beneficiary", 0);
		let arbiter = funded::<T>("arbiter", 0);

		#[extrinsic_call]
		_(RawOrigin::Signed(payer), beneficiary, Some(arbiter), milestones::<T>(m));

		assert!(Escrows::<T>::contains_key(0));
	}

	// Worst case: the final milestone, which also closes the escrow and reaps its account.
	#[benchmark]
	fn release() {
		let (id, payer, ..) = open::<T>();
		Pallet::<T>::release(RawOrigin::Signed(payer.clone()).into(), id)
			.expect("first milestone releases");

		#[extrinsic_call]
		_(RawOrigin::Signed(payer), id);

		assert!(!Escrows::<T>::contains_key(id));
	}

	#[benchmark]
	fn refund() {
		let (id, _, _, arbiter) = open::<T>();

		#[extrinsic_call]
		_(RawOrigin::Signed(arbiter), id);

		assert!(!Escrows::<T>::contains_key(id));
	}

	#[benchmark]
	fn cancel() {
		let (id, _, beneficiary, _) = open::<T>();

		#[extrinsic_call]
		_(RawOrigin::Signed(beneficiary), id);

		assert!(!Escrows::<T>::contains_key(id));
	}

	#[benchmark]
	fn submit() {
		let (id, _, beneficiary, _) = open::<T>();

		#[extrinsic_call]
		_(RawOrigin::Signed(beneficiary), id);

		assert!(Escrows::<T>::get(id).is_some_and(|e| e.claim.is_some()));
	}

	#[benchmark]
	fn dispute() {
		let (id, payer, ..) = last_submitted::<T>();

		#[extrinsic_call]
		_(RawOrigin::Signed(payer), id);

		assert!(Escrows::<T>::get(id).is_some_and(|e| e.claim.is_some_and(|c| c.disputed)));
	}

	// Worst case: the final milestone, which also closes the escrow.
	#[benchmark]
	fn claim() {
		let (id, _, beneficiary, _) = last_submitted::<T>();
		frame_system::Pallet::<T>::set_block_number(
			now::<T>().saturating_add(T::ChallengePeriod::get()),
		);

		#[extrinsic_call]
		_(RawOrigin::Signed(beneficiary), id);

		assert!(!Escrows::<T>::contains_key(id));
	}

	// Worst case: both sides paid and the escrow closed.
	#[benchmark]
	fn resolve() {
		let (id, payer, _, arbiter) = last_submitted::<T>();
		Pallet::<T>::dispute(RawOrigin::Signed(payer).into(), id).expect("claim disputes");
		let share = T::MinMilestone::get().saturating_mul(5u32.into());

		#[extrinsic_call]
		_(RawOrigin::Signed(arbiter), id, share);

		assert!(!Escrows::<T>::contains_key(id));
	}

	impl_benchmark_test_suite!(Pallet, crate::mock::new_test_ext(), crate::mock::Test);
}
