//! Storage migrations.

pub mod v1 {
	//! Storage version 0 is the hold-based layout of `pallet-escrow-v1`: each escrow's funds and
	//! deposit on hold on the payer's account, one deadline per escrow, and a count of live
	//! escrows per payer. Version 1 moves the funds and deposit into the escrow's own account,
	//! gives every milestone the old deadline and drops the count.
	//!
	//! It runs in a single block and touches every escrow once. A chain with more escrows than
	//! fit in one block has to split it across blocks.

	use crate::pallet::{BalanceOf, Config, EscrowId, EscrowInfo, HoldReason, Milestone, Pallet};
	use core::marker::PhantomData;
	use frame_support::{
		migrations::VersionedMigration,
		pallet_prelude::*,
		traits::{
			fungible::{Inspect, Mutate, MutateHold},
			tokens::{Fortitude, Precision, Preservation, Restriction},
			UncheckedOnRuntimeUpgrade,
		},
		BoundedVec,
	};
	use frame_system::pallet_prelude::BlockNumberFor;
	use sp_runtime::traits::{Saturating, Zero};

	#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
	#[scale_info(skip_type_params(T))]
	#[codec(mel_bound())]
	pub struct OldEscrowInfo<T: Config> {
		pub payer: T::AccountId,
		pub beneficiary: T::AccountId,
		pub arbiter: Option<T::AccountId>,
		pub milestones: BoundedVec<BalanceOf<T>, T::MaxMilestones>,
		pub released: u32,
		pub remaining: BalanceOf<T>,
		pub deposit: BalanceOf<T>,
		pub deadline: BlockNumberFor<T>,
	}

	#[frame_support::storage_alias]
	pub type Escrows<T: Config> = StorageMap<Pallet<T>, Twox64Concat, EscrowId, OldEscrowInfo<T>>;

	#[frame_support::storage_alias]
	pub type EscrowCount<T: Config> = StorageMap<
		Pallet<T>,
		Blake2_128Concat,
		<T as frame_system::Config>::AccountId,
		u32,
		ValueQuery,
	>;

	/// Moves storage version 0 to 1. Use through [`MigrateToV1`], which checks the version.
	pub struct UncheckedMigrateToV1<T>(PhantomData<T>);

	impl<T: Config> UncheckedMigrateToV1<T> {
		fn migrate(id: EscrowId, old: &OldEscrowInfo<T>) -> Result<EscrowInfo<T>, DispatchError> {
			let account = Pallet::<T>::account(id);
			// `Force`: the payer's locks cover held funds too (ESC-11), and these funds were
			// promised to the escrow before any of them.
			for (reason, amount) in
				[(HoldReason::Escrow, old.remaining), (HoldReason::StorageDeposit, old.deposit)]
			{
				if !amount.is_zero() {
					T::Currency::transfer_on_hold(
						&reason.into(),
						&old.payer,
						&account,
						amount,
						Precision::Exact,
						Restriction::Free,
						Fortitude::Force,
					)?;
				}
			}
			// Version 0 did not require the deposit to cover the existential deposit. The
			// escrow account now depends on it to outlive the last payout.
			let deposit = old.deposit.max(T::Currency::minimum_balance());
			let shortfall = deposit.saturating_sub(old.deposit);
			if !shortfall.is_zero() {
				T::Currency::transfer(&old.payer, &account, shortfall, Preservation::Expendable)?;
			}
			let milestones = old
				.milestones
				.iter()
				.map(|amount| Milestone { amount: *amount, deadline: old.deadline })
				.collect::<alloc::vec::Vec<_>>();
			Ok(EscrowInfo {
				payer: old.payer.clone(),
				beneficiary: old.beneficiary.clone(),
				arbiter: old.arbiter.clone(),
				milestones: BoundedVec::truncate_from(milestones),
				released: old.released,
				remaining: old.remaining,
				deposit,
				claim: None,
			})
		}

		/// Last resort for an escrow that cannot be moved: release its holds to the payer, as a
		/// refund would, rather than leave funds on hold with no record to release them.
		fn refund(old: &OldEscrowInfo<T>) {
			for (reason, amount) in
				[(HoldReason::Escrow, old.remaining), (HoldReason::StorageDeposit, old.deposit)]
			{
				// `Exact`: the hold is shared with the payer's other escrows (ESC-10).
				let _ = T::Currency::release(&reason.into(), &old.payer, amount, Precision::Exact);
			}
		}
	}

	impl<T: Config> UncheckedOnRuntimeUpgrade for UncheckedMigrateToV1<T> {
		fn on_runtime_upgrade() -> Weight {
			let mut escrows = 0u64;
			crate::pallet::Escrows::<T>::translate::<OldEscrowInfo<T>, _>(|id, old| {
				escrows.saturating_inc();
				let moved = frame_support::storage::with_storage_layer(|| Self::migrate(id, &old));
				match moved {
					Ok(new) => Some(new),
					Err(_) => {
						frame_support::defensive!("escrow could not be moved; refunded instead");
						Self::refund(&old);
						None
					},
				}
			});
			let counts = EscrowCount::<T>::clear(u32::MAX, None).unique;
			// Per escrow: the record, the payer's account and holds, the escrow account.
			T::DbWeight::get()
				.reads_writes(4, 4)
				.saturating_mul(escrows)
				.saturating_add(T::DbWeight::get().writes(u64::from(counts)))
		}

		#[cfg(feature = "try-runtime")]
		fn pre_upgrade() -> Result<alloc::vec::Vec<u8>, sp_runtime::TryRuntimeError> {
			let escrows: alloc::vec::Vec<(EscrowId, T::AccountId)> =
				Escrows::<T>::iter().map(|(id, old)| (id, old.payer)).collect();
			Ok(escrows.encode())
		}

		#[cfg(feature = "try-runtime")]
		fn post_upgrade(state: alloc::vec::Vec<u8>) -> Result<(), sp_runtime::TryRuntimeError> {
			use frame_support::traits::fungible::InspectHold;
			let escrows: alloc::vec::Vec<(EscrowId, T::AccountId)> =
				Decode::decode(&mut &state[..]).map_err(|_| "pre_upgrade state does not decode")?;
			for (id, payer) in escrows {
				ensure!(crate::pallet::Escrows::<T>::contains_key(id), "escrow lost in migration");
				for reason in [HoldReason::Escrow, HoldReason::StorageDeposit] {
					ensure!(
						T::Currency::balance_on_hold(&reason.into(), &payer).is_zero(),
						"hold left on a payer after migration"
					);
				}
			}
			ensure!(EscrowCount::<T>::iter().next().is_none(), "escrow counts left");
			Pallet::<T>::do_try_state()
		}
	}

	/// Moves storage version 0 to 1, then records version 1.
	pub type MigrateToV1<T> = VersionedMigration<
		0,
		1,
		UncheckedMigrateToV1<T>,
		Pallet<T>,
		<T as frame_system::Config>::DbWeight,
	>;
}
