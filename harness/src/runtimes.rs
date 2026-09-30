//! One mock runtime per pallet under test, both behind the same [`Target`] interface.

use crate::spec::{Record, SpecEvent, SpecParams, Target, UNDECODABLE};
use sp_runtime::DispatchResult;
use std::collections::BTreeMap;

pub const ACCOUNTS: u64 = 5;
pub const START_BALANCE: u64 = 10_000;
/// `pallet_balances::config_preludes::TestDefaultConfig::ExistentialDeposit`.
pub const EXISTENTIAL_DEPOSIT: u64 = 1;

fn genesis_balances() -> Vec<(u64, u64)> {
	(1..=ACCOUNTS).map(|who| (who, START_BALANCE)).collect()
}

pub mod fixed {
	use super::*;
	use frame_support::{
		derive_impl,
		traits::{
			fungible::{Inspect, InspectHold},
			ConstU32, ConstU64,
		},
		BoundedVec,
	};
	use sp_runtime::{BuildStorage, DispatchError};

	type Block = frame_system::mocking::MockBlock<Runtime>;

	frame_support::construct_runtime!(
		pub enum Runtime {
			System: frame_system,
			Balances: pallet_balances,
			Escrow: pallet_escrow,
		}
	);

	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for Runtime {
		type Block = Block;
		type AccountData = pallet_balances::AccountData<u64>;
	}

	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for Runtime {
		type AccountStore = System;
	}

	pub const MIN_MILESTONE: u64 = 10;
	pub const DEPOSIT: u64 = 5;
	pub const MAX_MILESTONES: u32 = 8;

	impl pallet_escrow::Config for Runtime {
		type RuntimeEvent = RuntimeEvent;
		type Currency = Balances;
		type RuntimeHoldReason = RuntimeHoldReason;
		type MaxMilestones = ConstU32<MAX_MILESTONES>;
		type MinMilestone = ConstU64<MIN_MILESTONE>;
		type EscrowDeposit = ConstU64<DEPOSIT>;
		type WeightInfo = ();
	}

	/// The hardened pallet.
	pub struct Fixed;

	impl Target for Fixed {
		const NAME: &'static str = "pallet-escrow";
		const PARAMS: SpecParams = SpecParams {
			min_milestone: MIN_MILESTONE,
			deposit: DEPOSIT,
			max_milestones: MAX_MILESTONES as usize,
			existential_deposit: EXISTENTIAL_DEPOSIT,
		};

		fn new_ext() -> sp_io::TestExternalities {
			let mut storage =
				frame_system::GenesisConfig::<Runtime>::default().build_storage().unwrap();
			pallet_balances::GenesisConfig::<Runtime> {
				balances: genesis_balances(),
				..Default::default()
			}
			.assimilate_storage(&mut storage)
			.unwrap();
			let mut ext: sp_io::TestExternalities = storage.into();
			ext.execute_with(|| System::set_block_number(1));
			ext
		}

		fn create(
			payer: u64,
			beneficiary: u64,
			arbiter: Option<u64>,
			milestones: Vec<u64>,
			deadline: u64,
		) -> DispatchResult {
			// An oversized vector never decodes into a `BoundedVec` call argument, so the call
			// could not reach the pallet at all.
			let milestones: BoundedVec<u64, ConstU32<MAX_MILESTONES>> =
				milestones.try_into().map_err(|_| DispatchError::Other(UNDECODABLE))?;
			Escrow::create(RuntimeOrigin::signed(payer), beneficiary, arbiter, milestones, deadline)
		}
		fn release(who: u64, id: u32) -> DispatchResult {
			Escrow::release(RuntimeOrigin::signed(who), id)
		}
		fn refund(who: u64, id: u32) -> DispatchResult {
			Escrow::refund(RuntimeOrigin::signed(who), id)
		}
		fn cancel(who: u64, id: u32) -> DispatchResult {
			Escrow::cancel(RuntimeOrigin::signed(who), id)
		}
		fn block() -> u64 {
			System::block_number()
		}
		fn set_block(n: u64) {
			System::set_block_number(n)
		}
		fn free(who: u64) -> u64 {
			Balances::free_balance(who)
		}
		fn held_escrow(who: u64) -> u64 {
			Balances::balance_on_hold(&pallet_escrow::HoldReason::Escrow.into(), &who)
		}
		fn held_deposit(who: u64) -> u64 {
			Balances::balance_on_hold(&pallet_escrow::HoldReason::StorageDeposit.into(), &who)
		}
		fn total_issuance() -> u64 {
			<Balances as Inspect<u64>>::total_issuance()
		}
		fn next_id() -> u32 {
			pallet_escrow::NextEscrowId::<Runtime>::get()
		}
		fn escrows() -> BTreeMap<u32, Record> {
			pallet_escrow::Escrows::<Runtime>::iter()
				.map(|(id, e)| {
					let record = Record {
						payer: e.payer,
						beneficiary: e.beneficiary,
						arbiter: e.arbiter,
						milestones: e.milestones.into_inner(),
						released: e.released,
						remaining: e.remaining,
						deposit: e.deposit,
						deadline: e.deadline,
					};
					(id, record)
				})
				.collect()
		}
		fn events() -> Option<Vec<SpecEvent>> {
			use pallet_escrow::Event as E;
			let events = System::events().into_iter().filter_map(|r| match r.event {
				RuntimeEvent::Escrow(e) => Some(match e {
					E::Created { id, payer, beneficiary, total } => {
						SpecEvent::Created { id, payer, beneficiary, total }
					},
					E::MilestoneReleased { id, index, amount } => {
						SpecEvent::MilestoneReleased { id, index, amount }
					},
					E::Completed { id } => SpecEvent::Completed { id },
					E::Refunded { id, amount } => SpecEvent::Refunded { id, amount },
					E::Cancelled { id, amount } => SpecEvent::Cancelled { id, amount },
					E::__Ignore(..) => unreachable!("never constructed"),
				}),
				_ => None,
			});
			Some(events.collect())
		}
		fn reset_events() {
			System::reset_events()
		}
		fn try_state() -> Result<(), String> {
			Escrow::do_try_state().map_err(|e| format!("{e:?}"))
		}
	}
}

pub mod v0 {
	use super::*;
	use frame_support::{
		derive_impl,
		traits::fungible::{Inspect, InspectHold},
	};
	use sp_runtime::BuildStorage;

	type Block = frame_system::mocking::MockBlock<Runtime>;

	frame_support::construct_runtime!(
		pub enum Runtime {
			System: frame_system,
			Balances: pallet_balances,
			Escrow: pallet_escrow_v0,
		}
	);

	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for Runtime {
		type Block = Block;
		type AccountData = pallet_balances::AccountData<u64>;
	}

	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for Runtime {
		type AccountStore = System;
	}

	impl pallet_escrow_v0::Config for Runtime {
		type RuntimeEvent = RuntimeEvent;
		type Currency = Balances;
		type RuntimeHoldReason = RuntimeHoldReason;
	}

	/// The first draft, audited as-is.
	pub struct V0;

	impl Target for V0 {
		const NAME: &'static str = "pallet-escrow-v0";
		// v0 has no deposit and no configured bounds; hold it to the same written spec with the
		// weakest reasonable limits (non-zero milestones, the same maximum count).
		const PARAMS: SpecParams = SpecParams {
			min_milestone: 1,
			deposit: 0,
			max_milestones: fixed::MAX_MILESTONES as usize,
			existential_deposit: EXISTENTIAL_DEPOSIT,
		};

		fn new_ext() -> sp_io::TestExternalities {
			let mut storage =
				frame_system::GenesisConfig::<Runtime>::default().build_storage().unwrap();
			pallet_balances::GenesisConfig::<Runtime> {
				balances: genesis_balances(),
				..Default::default()
			}
			.assimilate_storage(&mut storage)
			.unwrap();
			let mut ext: sp_io::TestExternalities = storage.into();
			ext.execute_with(|| System::set_block_number(1));
			ext
		}

		fn create(
			payer: u64,
			beneficiary: u64,
			arbiter: Option<u64>,
			milestones: Vec<u64>,
			deadline: u64,
		) -> DispatchResult {
			Escrow::create(RuntimeOrigin::signed(payer), beneficiary, arbiter, milestones, deadline)
		}
		fn release(who: u64, id: u32) -> DispatchResult {
			Escrow::release(RuntimeOrigin::signed(who), id)
		}
		fn refund(who: u64, id: u32) -> DispatchResult {
			Escrow::refund(RuntimeOrigin::signed(who), id)
		}
		fn cancel(who: u64, id: u32) -> DispatchResult {
			Escrow::cancel(RuntimeOrigin::signed(who), id)
		}
		fn block() -> u64 {
			System::block_number()
		}
		fn set_block(n: u64) {
			System::set_block_number(n)
		}
		fn free(who: u64) -> u64 {
			Balances::free_balance(who)
		}
		fn held_escrow(who: u64) -> u64 {
			Balances::balance_on_hold(&pallet_escrow_v0::HoldReason::Escrow.into(), &who)
		}
		fn held_deposit(_who: u64) -> u64 {
			0
		}
		fn total_issuance() -> u64 {
			<Balances as Inspect<u64>>::total_issuance()
		}
		fn next_id() -> u32 {
			pallet_escrow_v0::NextEscrowId::<Runtime>::get()
		}
		fn escrows() -> BTreeMap<u32, Record> {
			pallet_escrow_v0::Escrows::<Runtime>::iter()
				.map(|(id, e)| {
					let record = Record {
						payer: e.payer,
						beneficiary: e.beneficiary,
						arbiter: e.arbiter,
						milestones: e.milestones,
						released: e.released,
						remaining: e.remaining,
						deposit: 0,
						deadline: e.deadline,
					};
					(id, record)
				})
				.collect()
		}
		// v0's events predate the spec: `Created` has no beneficiary, `Cancelled` no amount and
		// there is no `Completed`. Holding it to them would bury the fund-loss findings.
		fn events() -> Option<Vec<SpecEvent>> {
			None
		}
		fn reset_events() {}
	}
}
