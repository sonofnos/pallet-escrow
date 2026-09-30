//! # Escrow pallet
//!
//! Milestone escrow between a payer and a beneficiary, with an optional arbiter.
//!
//! The payer funds the escrow up front. Milestones are paid in order, each with its own deadline.
//! The payer (or the arbiter) can release the next milestone at any time. With an arbiter named,
//! the beneficiary does not have to wait for that: they `submit` the milestone as done, and
//! unless the payer disputes within `ChallengePeriod` blocks they `claim` it themselves. A
//! disputed milestone waits for the arbiter to `resolve` it, splitting it between the two sides
//! however the arbiter decides.
//!
//! Once a milestone's deadline passes with nothing submitted, the payer can take back what is
//! left. The arbiter can refund at any time, and the beneficiary can walk away (`cancel`) at any
//! time. Refunds always go to the payer, whoever triggers them.
//!
//! Without an arbiter there is no one to settle a dispute, so `submit` is unavailable and the
//! escrow trusts the payer to release.
//!
//! ## Custody
//!
//! Each escrow's funds and storage deposit sit in an account of its own, derived from
//! `PalletId` and the escrow id. Nothing can move them except this pallet, and no escrow can
//! reach another's funds, whatever the bookkeeping says: the ledger enforces the isolation.
//! Locks and freezes on the payer cannot touch them either, since they have left the payer's
//! account. The deposit, at least the existential deposit, keeps the escrow account alive until
//! the escrow closes and everything left in it goes back to the payer.
//!
//! The invariant checked by `try_state` is, for every live escrow:
//!
//! ```text
//! balance(escrow account) >= remaining + deposit
//! remaining               == sum of unpaid milestones
//! ```
//!
//! `>=`, not `==`: anyone can send funds to an escrow account. They go to the payer on close.
//!
//! See `audit/REPORT.md` for the findings that shaped this design, against the first draft
//! (`pallet-escrow-v0`) and the hold-based pallet it replaced (`pallet-escrow-v1`).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub use pallet::*;
pub use weights::WeightInfo;

pub mod migrations;
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
			fungible::{Inspect, Mutate, MutateHold},
			tokens::Preservation,
		},
		CloneNoBound, DebugNoBound, EqNoBound, PalletId, PartialEqNoBound,
	};
	use frame_system::pallet_prelude::*;
	use sp_runtime::traits::{AccountIdConversion, CheckedAdd, CheckedSub, Saturating, Zero};

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

	pub type EscrowId = u32;

	/// Storage layout version. 0 is the hold-based layout of `pallet-escrow-v1`, which shipped
	/// without one; [`crate::migrations::v1`] moves it here.
	pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

	#[pallet::pallet]
	#[pallet::storage_version(STORAGE_VERSION)]
	pub struct Pallet<T>(_);

	/// Reasons this pallet placed funds on hold. Only storage version 0 did; the migration
	/// releases them.
	#[pallet::composite_enum]
	pub enum HoldReason {
		Escrow,
		StorageDeposit,
	}

	#[pallet::config]
	pub trait Config: frame_system::Config {
		#[allow(deprecated)]
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		type Currency: Mutate<Self::AccountId>
			+ MutateHold<Self::AccountId, Reason = Self::RuntimeHoldReason>;

		type RuntimeHoldReason: From<HoldReason>;

		/// Derives each escrow's account. The runtime's `AccountId` must be long enough that
		/// no two escrow ids share one; checked in `integrity_test`.
		#[pallet::constant]
		type PalletId: Get<PalletId>;

		/// Upper bound on milestones per escrow. Bounds storage and the `create` weight.
		#[pallet::constant]
		type MaxMilestones: Get<u32>;

		/// Smallest milestone, and the smallest share `resolve` may pay either side. Must be
		/// at least the existential deposit so every payout can create its recipient's
		/// account; checked in `integrity_test`.
		#[pallet::constant]
		type MinMilestone: Get<BalanceOf<Self>>;

		/// Paid into the escrow account for as long as the escrow occupies storage. At least
		/// the existential deposit, so the account outlives its last payout.
		#[pallet::constant]
		type EscrowDeposit: Get<BalanceOf<Self>>;

		/// Blocks the payer has to dispute a submitted milestone.
		#[pallet::constant]
		type ChallengePeriod: Get<BlockNumberFor<Self>>;

		type WeightInfo: WeightInfo;
	}

	#[derive(
		Encode,
		Decode,
		DecodeWithMemTracking,
		Clone,
		Copy,
		PartialEq,
		Eq,
		Debug,
		TypeInfo,
		MaxEncodedLen,
	)]
	pub struct Milestone<Balance, BlockNumber> {
		pub amount: Balance,
		/// Until this block the beneficiary may submit the milestone; from it, if nothing was
		/// submitted, the payer may take back what is left.
		pub deadline: BlockNumber,
	}

	pub type MilestoneOf<T> = Milestone<BalanceOf<T>, BlockNumberFor<T>>;

	/// The beneficiary's claim that the next milestone is done.
	#[derive(
		Encode,
		Decode,
		DecodeWithMemTracking,
		Clone,
		Copy,
		PartialEq,
		Eq,
		Debug,
		TypeInfo,
		MaxEncodedLen,
	)]
	pub struct Claim<BlockNumber> {
		pub submitted: BlockNumber,
		pub disputed: bool,
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
		pub milestones: BoundedVec<MilestoneOf<T>, T::MaxMilestones>,
		/// Number of milestones already paid out.
		pub released: u32,
		/// Amount of unpaid milestones.
		pub remaining: BalanceOf<T>,
		/// Storage deposit, returned to the payer on close.
		pub deposit: BalanceOf<T>,
		/// A claim on milestone `released`, if the beneficiary has submitted it.
		pub claim: Option<Claim<BlockNumberFor<T>>>,
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
		Submitted {
			id: EscrowId,
			index: u32,
		},
		Disputed {
			id: EscrowId,
			index: u32,
		},
		Resolved {
			id: EscrowId,
			index: u32,
			to_beneficiary: BalanceOf<T>,
			to_payer: BalanceOf<T>,
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
		/// The first deadline is not in the future.
		DeadlineInPast,
		/// A milestone's deadline is before the one preceding it.
		DeadlinesOutOfOrder,
		/// The payer asked for a refund before the next milestone's deadline.
		DeadlineNotReached,
		/// Arithmetic overflow while totalling milestones.
		Overflow,
		/// Escrow ids are exhausted.
		IdsExhausted,
		/// Claims need an arbiter to settle disputes.
		NoArbiter,
		/// The next milestone has already been submitted.
		ClaimPending,
		/// The next milestone's deadline has passed; it can no longer be submitted.
		DeadlinePassed,
		/// The next milestone has not been submitted.
		NoClaim,
		/// The claim has already been disputed.
		AlreadyDisputed,
		/// The challenge period is over; the claim can no longer be disputed.
		ChallengeOver,
		/// The claim is disputed; only the arbiter can settle it.
		Disputed,
		/// The challenge period is still running.
		ChallengeNotOver,
		/// There is no disputed claim to resolve.
		NotDisputed,
		/// A share is larger than the milestone, or non-zero but below `MinMilestone`.
		InvalidSplit,
		/// Bookkeeping disagrees with itself. Should be unreachable.
		Inconsistent,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn integrity_test() {
			let ed = T::Currency::minimum_balance();
			assert!(T::MaxMilestones::get() > 0, "MaxMilestones must be non-zero");
			assert!(T::MinMilestone::get() >= ed, "MinMilestone must be at least the ED");
			assert!(T::EscrowDeposit::get() >= ed, "EscrowDeposit must be at least the ED");
			assert!(!T::ChallengePeriod::get().is_zero(), "ChallengePeriod must be non-zero");
			assert!(
				AccountIdConversion::<T::AccountId>::try_into_sub_account(
					&T::PalletId::get(),
					EscrowId::MAX
				)
				.is_some(),
				"AccountId too short: escrow accounts would collide"
			);
		}

		#[cfg(feature = "try-runtime")]
		fn try_state(_n: BlockNumberFor<T>) -> Result<(), sp_runtime::TryRuntimeError> {
			Self::do_try_state()
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Open an escrow and move its full value plus the storage deposit from the caller into
		/// the escrow's account.
		#[pallet::call_index(0)]
		#[pallet::weight(T::WeightInfo::create(milestones.len() as u32))]
		pub fn create(
			origin: OriginFor<T>,
			beneficiary: T::AccountId,
			arbiter: Option<T::AccountId>,
			milestones: BoundedVec<MilestoneOf<T>, T::MaxMilestones>,
		) -> DispatchResult {
			let payer = ensure_signed(origin)?;
			ensure!(payer != beneficiary, Error::<T>::SelfEscrow);
			if let Some(arbiter) = &arbiter {
				ensure!(*arbiter != payer && *arbiter != beneficiary, Error::<T>::ArbiterConflict);
			}
			let first = milestones.first().ok_or(Error::<T>::NoMilestones)?;
			ensure!(
				first.deadline > frame_system::Pallet::<T>::block_number(),
				Error::<T>::DeadlineInPast
			);
			ensure!(
				milestones.windows(2).all(|w| w[0].deadline <= w[1].deadline),
				Error::<T>::DeadlinesOutOfOrder
			);

			let min = T::MinMilestone::get();
			let mut total = BalanceOf::<T>::zero();
			for m in milestones.iter() {
				ensure!(m.amount >= min, Error::<T>::MilestoneTooSmall);
				total = total.checked_add(&m.amount).ok_or(Error::<T>::Overflow)?;
			}
			let deposit = T::EscrowDeposit::get();
			let funding = total.checked_add(&deposit).ok_or(Error::<T>::Overflow)?;

			let id = NextEscrowId::<T>::get();
			let next_id = id.checked_add(1).ok_or(Error::<T>::IdsExhausted)?;
			T::Currency::transfer(&payer, &Self::account(id), funding, Preservation::Preserve)?;

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
					claim: None,
				},
			);
			Self::deposit_event(Event::Created { id, payer, beneficiary, total });
			Ok(())
		}

		/// Pay the next milestone to the beneficiary. Payer or arbiter only, claim or no claim.
		#[pallet::call_index(1)]
		#[pallet::weight(T::WeightInfo::release())]
		pub fn release(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Self::escrow(id)?;
			ensure!(
				who == escrow.payer || escrow.arbiter.as_ref() == Some(&who),
				Error::<T>::NotAuthorized
			);
			Self::pay_next(id, escrow)
		}

		/// Return everything still held to the payer. The arbiter may do this at any time;
		/// the payer only once the next milestone's deadline has passed with nothing submitted.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::refund())]
		pub fn refund(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Self::escrow(id)?;
			if escrow.arbiter.as_ref() != Some(&who) {
				ensure!(who == escrow.payer, Error::<T>::NotAuthorized);
				ensure!(
					frame_system::Pallet::<T>::block_number() >= Self::next(&escrow)?.deadline,
					Error::<T>::DeadlineNotReached
				);
				ensure!(escrow.claim.is_none(), Error::<T>::ClaimPending);
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
			let escrow = Self::escrow(id)?;
			ensure!(who == escrow.beneficiary, Error::<T>::NotAuthorized);
			let amount = escrow.remaining;
			Self::close(id, &escrow)?;
			Self::deposit_event(Event::Cancelled { id, amount });
			Ok(())
		}

		/// The beneficiary declares the next milestone done. Unless the payer disputes it
		/// within `ChallengePeriod` blocks, the beneficiary can then `claim` it.
		#[pallet::call_index(4)]
		#[pallet::weight(T::WeightInfo::submit())]
		pub fn submit(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let mut escrow = Self::escrow(id)?;
			ensure!(who == escrow.beneficiary, Error::<T>::NotAuthorized);
			ensure!(escrow.arbiter.is_some(), Error::<T>::NoArbiter);
			ensure!(escrow.claim.is_none(), Error::<T>::ClaimPending);
			let now = frame_system::Pallet::<T>::block_number();
			ensure!(now < Self::next(&escrow)?.deadline, Error::<T>::DeadlinePassed);

			escrow.claim = Some(Claim { submitted: now, disputed: false });
			let index = escrow.released;
			Escrows::<T>::insert(id, escrow);
			Self::deposit_event(Event::Submitted { id, index });
			Ok(())
		}

		/// The payer contests a submitted milestone. It then waits for the arbiter.
		#[pallet::call_index(5)]
		#[pallet::weight(T::WeightInfo::dispute())]
		pub fn dispute(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let mut escrow = Self::escrow(id)?;
			ensure!(who == escrow.payer, Error::<T>::NotAuthorized);
			let claim = escrow.claim.as_mut().ok_or(Error::<T>::NoClaim)?;
			ensure!(!claim.disputed, Error::<T>::AlreadyDisputed);
			ensure!(
				frame_system::Pallet::<T>::block_number()
					< claim.submitted.saturating_add(T::ChallengePeriod::get()),
				Error::<T>::ChallengeOver
			);

			claim.disputed = true;
			let index = escrow.released;
			Escrows::<T>::insert(id, escrow);
			Self::deposit_event(Event::Disputed { id, index });
			Ok(())
		}

		/// The beneficiary collects a submitted milestone the payer did not dispute in time.
		#[pallet::call_index(6)]
		#[pallet::weight(T::WeightInfo::claim())]
		pub fn claim(origin: OriginFor<T>, id: EscrowId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Self::escrow(id)?;
			ensure!(who == escrow.beneficiary, Error::<T>::NotAuthorized);
			let claim = escrow.claim.ok_or(Error::<T>::NoClaim)?;
			ensure!(!claim.disputed, Error::<T>::Disputed);
			ensure!(
				frame_system::Pallet::<T>::block_number()
					>= claim.submitted.saturating_add(T::ChallengePeriod::get()),
				Error::<T>::ChallengeNotOver
			);
			Self::pay_next(id, escrow)
		}

		/// The arbiter settles a disputed milestone: `to_beneficiary` goes to the beneficiary
		/// and the rest of the milestone back to the payer. Each share is zero or at least
		/// `MinMilestone`.
		#[pallet::call_index(7)]
		#[pallet::weight(T::WeightInfo::resolve())]
		pub fn resolve(
			origin: OriginFor<T>,
			id: EscrowId,
			to_beneficiary: BalanceOf<T>,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let mut escrow = Self::escrow(id)?;
			ensure!(escrow.arbiter.as_ref() == Some(&who), Error::<T>::NotAuthorized);
			ensure!(escrow.claim.is_some_and(|c| c.disputed), Error::<T>::NotDisputed);

			let amount = Self::next(&escrow)?.amount;
			let to_payer = amount.checked_sub(&to_beneficiary).ok_or(Error::<T>::InvalidSplit)?;
			let min = T::MinMilestone::get();
			for share in [to_beneficiary, to_payer] {
				ensure!(share.is_zero() || share >= min, Error::<T>::InvalidSplit);
			}

			let account = Self::account(id);
			for (to, share) in [(&escrow.beneficiary, to_beneficiary), (&escrow.payer, to_payer)] {
				if !share.is_zero() {
					T::Currency::transfer(&account, to, share, Preservation::Preserve)?;
				}
			}
			let index = escrow.released;
			Self::deposit_event(Event::Resolved { id, index, to_beneficiary, to_payer });
			Self::advance(id, &mut escrow, amount)
		}
	}

	impl<T: Config> Pallet<T> {
		/// The account holding escrow `id`'s funds.
		pub fn account(id: EscrowId) -> T::AccountId {
			T::PalletId::get().into_sub_account_truncating(id)
		}

		fn escrow(id: EscrowId) -> Result<EscrowInfo<T>, DispatchError> {
			Ok(Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?)
		}

		/// The next unpaid milestone. A fully paid escrow is closed, so a live one always has
		/// one; anything else is a bookkeeping error, never a panic.
		fn next(escrow: &EscrowInfo<T>) -> Result<MilestoneOf<T>, DispatchError> {
			Ok(*escrow.milestones.get(escrow.released as usize).ok_or(Error::<T>::Inconsistent)?)
		}

		fn pay_next(id: EscrowId, mut escrow: EscrowInfo<T>) -> DispatchResult {
			let amount = Self::next(&escrow)?.amount;
			T::Currency::transfer(
				&Self::account(id),
				&escrow.beneficiary,
				amount,
				Preservation::Preserve,
			)?;
			let index = escrow.released;
			Self::deposit_event(Event::MilestoneReleased { id, index, amount });
			Self::advance(id, &mut escrow, amount)
		}

		/// Mark the next milestone, worth `amount`, as settled and close the escrow if it was
		/// the last.
		fn advance(
			id: EscrowId,
			escrow: &mut EscrowInfo<T>,
			amount: BalanceOf<T>,
		) -> DispatchResult {
			escrow.remaining =
				escrow.remaining.checked_sub(&amount).ok_or(Error::<T>::Inconsistent)?;
			escrow.released = escrow.released.checked_add(1).ok_or(Error::<T>::Inconsistent)?;
			escrow.claim = None;
			if escrow.released as usize == escrow.milestones.len() {
				ensure!(escrow.remaining.is_zero(), Error::<T>::Inconsistent);
				Self::close(id, escrow)?;
				Self::deposit_event(Event::Completed { id });
			} else {
				Escrows::<T>::insert(id, escrow);
			}
			Ok(())
		}

		/// Send everything in the escrow's account to the payer and delete the escrow. The
		/// account is reaped.
		fn close(id: EscrowId, escrow: &EscrowInfo<T>) -> DispatchResult {
			let account = Self::account(id);
			let owed = escrow.remaining.saturating_add(escrow.deposit);
			let balance = T::Currency::balance(&account);
			ensure!(balance >= owed, Error::<T>::Inconsistent);
			T::Currency::transfer(&account, &escrow.payer, balance, Preservation::Expendable)?;
			Escrows::<T>::remove(id);
			Ok(())
		}

		/// Check the custody invariant described in the crate docs.
		#[cfg(any(feature = "try-runtime", test))]
		pub fn do_try_state() -> Result<(), sp_runtime::TryRuntimeError> {
			ensure!(
				Pallet::<T>::on_chain_storage_version() == STORAGE_VERSION,
				"storage version not migrated"
			);
			let next_id = NextEscrowId::<T>::get();
			for (id, escrow) in Escrows::<T>::iter() {
				ensure!(id < next_id, "escrow id at or above NextEscrowId");
				ensure!(!escrow.milestones.is_empty(), "live escrow without milestones");
				ensure!(
					(escrow.released as usize) < escrow.milestones.len(),
					"fully released escrow left in storage"
				);
				ensure!(
					escrow.claim.is_none() || escrow.arbiter.is_some(),
					"claim on an escrow without an arbiter"
				);
				let mut unpaid = BalanceOf::<T>::zero();
				for m in escrow.milestones.iter().skip(escrow.released as usize) {
					unpaid = unpaid.checked_add(&m.amount).ok_or("milestone sum overflows")?;
				}
				ensure!(unpaid == escrow.remaining, "remaining differs from unpaid milestones");
				let owed = unpaid.checked_add(&escrow.deposit).ok_or("owed overflows")?;
				ensure!(
					T::Currency::balance(&Self::account(id)) >= owed,
					"escrow account holds less than the escrow owes"
				);
			}
			Ok(())
		}
	}
}
