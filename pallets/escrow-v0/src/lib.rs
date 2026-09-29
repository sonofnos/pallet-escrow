//! # Escrow pallet, first draft (AUDIT TARGET)
//!
//! This is the unreviewed first version of `pallet-escrow`, kept byte-for-byte so every finding
//! in `audit/REPORT.md` can be reproduced against it (`cargo test -p escrow-harness`). It holds
//! the bug classes most often found in FRAME audits. **Do not use it in a runtime.**

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
	use alloc::vec::Vec;
	use frame_support::{
		pallet_prelude::*,
		traits::{
			fungible::{Inspect, Mutate, MutateHold},
			tokens::{Fortitude, Precision, Restriction},
		},
	};
	use frame_system::pallet_prelude::*;
	use sp_runtime::traits::Zero;

	pub type BalanceOf<T> =
		<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

	#[pallet::pallet]
	#[pallet::without_storage_info]
	pub struct Pallet<T>(_);

	#[pallet::composite_enum]
	pub enum HoldReason {
		Escrow,
	}

	#[pallet::config]
	pub trait Config: frame_system::Config {
		#[allow(deprecated)]
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
		type Currency: Mutate<Self::AccountId>
			+ MutateHold<Self::AccountId, Reason = Self::RuntimeHoldReason>;
		type RuntimeHoldReason: From<HoldReason>;
	}

	#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo)]
	pub struct EscrowInfo<AccountId, Balance, BlockNumber> {
		pub payer: AccountId,
		pub beneficiary: AccountId,
		pub arbiter: Option<AccountId>,
		pub milestones: Vec<Balance>,
		pub released: u32,
		pub remaining: Balance,
		pub deadline: BlockNumber,
	}

	pub type EscrowOf<T> =
		EscrowInfo<<T as frame_system::Config>::AccountId, BalanceOf<T>, BlockNumberFor<T>>;

	#[pallet::storage]
	pub type Escrows<T: Config> = StorageMap<_, Twox64Concat, u32, EscrowOf<T>>;

	#[pallet::storage]
	pub type NextEscrowId<T: Config> = StorageValue<_, u32, ValueQuery>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		Created { id: u32, payer: T::AccountId, total: BalanceOf<T> },
		Released { id: u32, amount: BalanceOf<T> },
		Refunded { id: u32, amount: BalanceOf<T> },
		Cancelled { id: u32 },
	}

	#[pallet::error]
	pub enum Error<T> {
		NotAuthorized,
		NotFound,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::call_index(0)]
		#[pallet::weight(Weight::zero())]
		pub fn create(
			origin: OriginFor<T>,
			beneficiary: T::AccountId,
			arbiter: Option<T::AccountId>,
			milestones: Vec<BalanceOf<T>>,
			deadline: BlockNumberFor<T>,
		) -> DispatchResult {
			let payer = ensure_signed(origin)?;
			let total = milestones.iter().fold(BalanceOf::<T>::zero(), |acc, m| acc + *m);
			T::Currency::hold(&HoldReason::Escrow.into(), &payer, total)?;

			let id = NextEscrowId::<T>::get();
			NextEscrowId::<T>::put(id + 1);
			Escrows::<T>::insert(
				id,
				EscrowInfo {
					payer: payer.clone(),
					beneficiary,
					arbiter,
					milestones,
					released: 0,
					remaining: total,
					deadline,
				},
			);
			Self::deposit_event(Event::Created { id, payer, total });
			Ok(())
		}

		#[pallet::call_index(1)]
		#[pallet::weight(Weight::zero())]
		pub fn release(origin: OriginFor<T>, id: u32) -> DispatchResult {
			ensure_signed(origin)?;
			let mut escrow = Escrows::<T>::get(id).unwrap();
			let amount = escrow.milestones[escrow.released as usize];
			T::Currency::transfer_on_hold(
				&HoldReason::Escrow.into(),
				&escrow.payer,
				&escrow.beneficiary,
				amount,
				Precision::BestEffort,
				Restriction::Free,
				Fortitude::Polite,
			)?;
			escrow.released += 1;
			escrow.remaining = escrow.remaining - amount;
			Escrows::<T>::insert(id, escrow);
			Self::deposit_event(Event::Released { id, amount });
			Ok(())
		}

		#[pallet::call_index(2)]
		#[pallet::weight(Weight::zero())]
		pub fn refund(origin: OriginFor<T>, id: u32) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?;
			ensure!(
				who == escrow.payer || Some(who.clone()) == escrow.arbiter,
				Error::<T>::NotAuthorized
			);
			T::Currency::transfer_on_hold(
				&HoldReason::Escrow.into(),
				&escrow.payer,
				&who,
				escrow.remaining,
				Precision::BestEffort,
				Restriction::Free,
				Fortitude::Polite,
			)?;
			Escrows::<T>::remove(id);
			Self::deposit_event(Event::Refunded { id, amount: escrow.remaining });
			Ok(())
		}

		#[pallet::call_index(3)]
		#[pallet::weight(Weight::zero())]
		pub fn cancel(origin: OriginFor<T>, id: u32) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let escrow = Escrows::<T>::get(id).ok_or(Error::<T>::NotFound)?;
			ensure!(who == escrow.beneficiary, Error::<T>::NotAuthorized);
			T::Currency::release(
				&HoldReason::Escrow.into(),
				&escrow.payer,
				escrow.remaining,
				Precision::BestEffort,
			)?;
			Self::deposit_event(Event::Cancelled { id });
			Ok(())
		}
	}
}
