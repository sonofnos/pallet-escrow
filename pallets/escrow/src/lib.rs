//! # Escrow pallet
//!
//! Milestone escrow between a payer and a beneficiary, with an optional arbiter.
//!
//! The payer locks the full amount up front as a fungible hold. Each `release` pays the next
//! milestone to the beneficiary. The payer can reclaim what is left after the deadline, the
//! arbiter can refund at any time, and the beneficiary can cancel (walk away) at any time.
//!
//! ## Accounting model
//!
//! Holds in `pallet-balances` are keyed by `(account, reason)`, not by escrow. Every escrow a
//! payer opens adds to the same `HoldReason::Escrow` balance, so the pallet must never release
//! or transfer more than one escrow's own `remaining`. The invariant checked by `try_state` is:
//!
//! ```text
//! balance_on_hold(Escrow, payer)         == sum(remaining)  over the payer's live escrows
//! balance_on_hold(StorageDeposit, payer) == sum(deposit)    over the payer's live escrows
//! ```
//!
//! See `audit/REPORT.md` for the findings against the first draft (`pallet-escrow-v0`) that
//! shaped this version.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub use pallet::*;
pub use weights::WeightInfo;

pub mod weights;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[frame_support::pallet]
pub mod pallet {
	use super::WeightInfo;
	use frame_support::{
		pallet_prelude::*,
		traits::{
			fungible::{Inspect, InspectHold, Mutate, MutateHold},
			tokens::{Fortitude, Precision, Restriction},
		},
		CloneNoBound, DebugNoBound, EqNoBound, PartialEqNoBound,
	};
	use frame_system::pallet_prelude::*;
	use sp_runtime::traits::{CheckedAdd, CheckedSub, Zero};

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

	pub type EscrowId = u32;

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	/// Reasons this pallet places funds on hold.
	#[pallet::composite_enum]
	pub enum HoldReason {
		/// Funds promised to a beneficiary.
		Escrow,
		/// Refundable deposit that pays for the escrow's storage.
		StorageDeposit,
	}

	#[pallet::config]
	pub trait Config: frame_system::Config {
		#[allow(deprecated)]
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		type Currency: Mutate<Self::AccountId>
			+ MutateHold<Self::AccountId, Reason = Self::RuntimeHoldReason>
			+ InspectHold<Self::AccountId>;

		type RuntimeHoldReason: From<HoldReason>;

		/// Upper bound on milestones per escrow. Bounds storage and the `create` weight.
		#[pallet::constant]
		type MaxMilestones: Get<u32>;

		/// Smallest milestone. Must be at least the existential deposit so a payout can always
		/// create the beneficiary's account; checked in `integrity_test`.
		#[pallet::constant]
		type MinMilestone: Get<BalanceOf<Self>>;

		/// Held from the payer for as long as the escrow occupies storage.
		#[pallet::constant]
		type EscrowDeposit: Get<BalanceOf<Self>>;

		type WeightInfo: WeightInfo;
	}

	#[derive(
		Encode,
		Decode,
		CloneNoBound,
		PartialEqNoBound,
		EqNoBound,
		DebugNoBound,
		TypeInfo,
		MaxEncodedLen,
	)]
	#[scale_info(skip_type_params(T))]
	#[codec(mel_bound())]
	pub struct EscrowInfo<T: Config> {
		pub payer: T::AccountId,
		pub beneficiary: T::AccountId,
		pub arbiter: Option<T::AccountId>,
		pub milestones: BoundedVec<BalanceOf<T>, T::MaxMilestones>,
		/// Number of milestones already paid out.
		pub released: u32,
		/// Amount still on hold for this escrow.
		pub remaining: BalanceOf<T>,
		/// Storage deposit held for this escrow.
		pub deposit: BalanceOf<T>,
		/// From this block the payer may reclaim `remaining`.
		pub deadline: BlockNumberFor<T>,
	}

	#[pallet::storage]
	pub type Escrows<T: Config> = StorageMap<_, Twox64Concat, EscrowId, EscrowInfo<T>>;

	#[pallet::storage]
	pub type NextEscrowId<T: Config> = StorageValue<_, EscrowId, ValueQuery>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		Created {
			id: EscrowId,
			payer: T::AccountId,
			beneficiary: T::AccountId,
			total: BalanceOf<T>,
		},
		MilestoneReleased {
			id: EscrowId,
			index: u32,
			amount: BalanceOf<T>,
		},
		Completed {
			id: EscrowId,
		},
		Refunded {
			id: EscrowId,
			amount: BalanceOf<T>,
		},
		Cancelled {
			id: EscrowId,
			amount: BalanceOf<T>,
		},
	}

	#[pallet::error]
	pub enum Error<T> {
		/// No live escrow with this id.
		NotFound,
		/// The caller may not perform this action on this escrow.
		NotAuthorized,
		/// An escrow needs at least one milestone.
		NoMilestones,
		/// A milestone is below `MinMilestone`.
		MilestoneTooSmall,
		/// Payer and beneficiary are the same account.
		SelfEscrow,
		/// The arbiter must be a third party.
		ArbiterConflict,
		/// The deadline is not in the future.
		DeadlineInPast,
		/// The payer asked for a refund before the deadline.
		DeadlineNotReached,
		/// Arithmetic overflow while totalling milestones.
		Overflow,
		/// Escrow ids are exhausted.
		IdsExhausted,
		/// Bookkeeping disagrees with itself. Should be unreachable.
		Inconsistent,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn integrity_test() {
			assert!(T::MaxMilestones::get() > 0, "MaxMilestones must be non-zero");
			assert!(
				T::MinMilestone::get() >= T::Currency::minimum_balance(),
				"MinMilestone must be at least the existential deposit"
			);
		}

		#[cfg(feature = "try-runtime")]
		fn try_state(_n: BlockNumberFor<T>) -> Result<(), sp_runtime::TryRuntimeError> {
			Self::do_try_state()
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Open an escrow and hold its full value plus the storage deposit from the caller.
		#[pallet::call_index(0)]
		#[pallet::weight(T::WeightInfo::create(T::MaxMilestones::get()))]
		pub fn create(
			origin: OriginFor<T>,
			beneficiary: T::AccountId,
			arbiter: Option<T::AccountId>,
			milestones: BoundedVec<BalanceOf<T>, T::MaxMilestones>,
			deadline: BlockNumberFor<T>,
		) -> DispatchResult {
			let payer = ensure_signed(origin)?;
			ensure!(payer != beneficiary, Error::<T>::SelfEscrow);
			if let Some(arbiter) = &arbiter {
				ensure!(*arbiter != payer && *arbiter != beneficiary, Error::<T>::ArbiterConflict);
			}
			ensure!(!milestones.is_empty(), Error::<T>::NoMilestones);
			ensure!(
				deadline > frame_system::Pallet::<T>::block_number(),
				Error::<T>::DeadlineInPast
			);

			let min = T::MinMilestone::get();
			let mut total = BalanceOf::<T>::zero();
			for amount in milestones.iter() {
				ensure!(*amount >= min, Error::<T>::MilestoneTooSmall);
				total = total.checked_add(amount).ok_or(Error::<T>::Overflow)?;
			}

			let id = NextEscrowId::<T>::get();
			let next_id = id.checked_add(1).ok_or(Error::<T>::IdsExhausted)?;
			let deposit = T::EscrowDeposit::get();

			// Dispatchables are transactional: if the second hold fails, the first is reverted.
			T::Currency::hold(&HoldReason::Escrow.into(), &payer, total)?;
			T::Currency::hold(&HoldReason::StorageDeposit.into(), &payer, deposit)?;

			NextEscrowId::<T>::put(next_id);
			Escrows::<T>::insert(
				id,
				EscrowInfo {
					payer: payer.clone(),
					beneficiary: beneficiary.clone(),
					arbiter,
					milestones,
					released: 0,
					remaining: total,
					deposit,
					deadline,
				},
			);
			Self::deposit_event(Event::Created { id, payer, beneficiary, total });
			Ok(())
		}

		/// Pay the next milestone to the beneficiary. Payer or arbiter only.
		#[pallet::call_index(1)]
		#[pallet::weight(T::WeightInfo::release())]
		pub fn release(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let mut escrow = Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?;
			ensure!(
				who == escrow.payer || escrow.arbiter.as_ref() == Some(&who),
				Error::<T>::NotAuthorized
			);

			let index = escrow.released;
			// A fully released escrow is removed below, so a live escrow always has a next
			// milestone. Treat anything else as a bookkeeping error, never a panic.
			let amount = *escrow.milestones.get(index as usize).ok_or(Error::<T>::Inconsistent)?;
			let remaining =
				escrow.remaining.checked_sub(&amount).ok_or(Error::<T>::Inconsistent)?;

			T::Currency::transfer_on_hold(
				&HoldReason::Escrow.into(),
				&escrow.payer,
				&escrow.beneficiary,
				amount,
				Precision::Exact,
				Restriction::Free,
				Fortitude::Polite,
			)?;

			escrow.released = index.checked_add(1).ok_or(Error::<T>::Inconsistent)?;
			escrow.remaining = remaining;
			Self::deposit_event(Event::MilestoneReleased { id, index, amount });

			if escrow.released as usize == escrow.milestones.len() {
				ensure!(escrow.remaining.is_zero(), Error::<T>::Inconsistent);
				Self::close(id, &escrow)?;
				Self::deposit_event(Event::Completed { id });
			} else {
				Escrows::<T>::insert(id, escrow);
			}
			Ok(())
		}

		/// Return everything still held to the payer. The arbiter may do this at any time,
		/// the payer only once the deadline has passed.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::refund())]
		pub fn refund(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?;
			if escrow.arbiter.as_ref() != Some(&who) {
				ensure!(who == escrow.payer, Error::<T>::NotAuthorized);
				ensure!(
					frame_system::Pallet::<T>::block_number() >= escrow.deadline,
					Error::<T>::DeadlineNotReached
				);
			}
			let amount = escrow.remaining;
			Self::close(id, &escrow)?;
			Self::deposit_event(Event::Refunded { id, amount });
			Ok(())
		}

		/// The beneficiary gives up the escrow and everything still held returns to the payer.
		#[pallet::call_index(3)]
		#[pallet::weight(T::WeightInfo::cancel())]
		pub fn cancel(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?;
			ensure!(who == escrow.beneficiary, Error::<T>::NotAuthorized);
			let amount = escrow.remaining;
			Self::close(id, &escrow)?;
			Self::deposit_event(Event::Cancelled { id, amount });
			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {
		/// Release this escrow's own funds and deposit to the payer and delete it.
		///
		/// `Precision::Exact` matters: the payer's hold is shared with their other escrows, so a
		/// best-effort release of a stale amount would silently eat into them.
		fn close(id: EscrowId, escrow: &EscrowInfo<T>) -> DispatchResult {
			T::Currency::release(
				&HoldReason::Escrow.into(),
				&escrow.payer,
				escrow.remaining,
				Precision::Exact,
			)?;
			T::Currency::release(
				&HoldReason::StorageDeposit.into(),
				&escrow.payer,
				escrow.deposit,
				Precision::Exact,
			)?;
			Escrows::<T>::remove(id);
			Ok(())
		}

		/// Check the accounting invariant described in the crate docs.
		#[cfg(any(feature = "try-runtime", test))]
		pub fn do_try_state() -> Result<(), sp_runtime::TryRuntimeError> {
			use alloc::collections::btree_map::BTreeMap;

			let mut expected: BTreeMap<T::AccountId, (BalanceOf<T>, BalanceOf<T>)> =
				BTreeMap::new();
			let next_id = NextEscrowId::<T>::get();
			for (id, escrow) in Escrows::<T>::iter() {
				ensure!(id < next_id, "escrow id at or above NextEscrowId");
				ensure!(!escrow.milestones.is_empty(), "live escrow without milestones");
				ensure!(
					(escrow.released as usize) < escrow.milestones.len(),
					"fully released escrow left in storage"
				);
				let mut unpaid = BalanceOf::<T>::zero();
				for amount in escrow.milestones.iter().skip(escrow.released as usize) {
					unpaid = unpaid.checked_add(amount).ok_or("milestone sum overflows")?;
				}
				ensure!(unpaid == escrow.remaining, "remaining differs from unpaid milestones");

				let entry = expected.entry(escrow.payer.clone()).or_default();
				entry.0 = entry.0.checked_add(&escrow.remaining).ok_or("hold sum overflows")?;
				entry.1 = entry.1.checked_add(&escrow.deposit).ok_or("deposit sum overflows")?;
			}
			for (payer, (funds, deposits)) in expected {
				ensure!(
					T::Currency::balance_on_hold(&HoldReason::Escrow.into(), &payer) == funds,
					"escrow hold differs from the payer's live escrows"
				);
				ensure!(
					T::Currency::balance_on_hold(&HoldReason::StorageDeposit.into(), &payer)
						== deposits,
					"deposit hold differs from the payer's live escrows"
				);
			}
			Ok(())
		}
	}
}
