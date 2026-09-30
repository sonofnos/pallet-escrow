//! One mock runtime per pallet under test, all behind the same [`Target`] interface.
//!
//! `v0` runs with `u64` balances and accounts, as it was audited. The hardened pallets run
//! with `u128` for both, as a production chain would; `v2` needs `u128` accounts anyway, since a
//! `u64` account is too short to derive a distinct account per escrow.

use crate::spec::{Balance, Claim, Custody, Record, SpecEvent, SpecParams, Target, UNDECODABLE};
use sp_runtime::DispatchResult;
use std::collections::BTreeMap;

pub const ACCOUNTS: u64 = 5;
pub const START_BALANCE: u64 = 10_000;
/// `pallet_balances::config_preludes::TestDefaultConfig::ExistentialDeposit`, kept for every
/// runtime.
pub const EXISTENTIAL_DEPOSIT: u64 = 1;

/// The ledger side of a [`Target`], identical for every runtime: blocks, balances, and what
/// other pallets do to the same accounts. Expects `Runtime`, `System`, `Balances` and an
/// `acc(u64) -> AccountId` in scope.
macro_rules! ledger {
	() => {
		fn new_ext() -> sp_io::TestExternalities {
			// The whole runtime's genesis, so storage versions are recorded as on a chain.
			let storage = RuntimeGenesisConfig {
				balances: pallet_balances::GenesisConfig {
					balances: (1..=ACCOUNTS).map(|who| (acc(who), START_BALANCE.into())).collect(),
					..Default::default()
				},
				..Default::default()
			}
			.build_storage()
			.unwrap();
			let mut ext: sp_io::TestExternalities = storage.into();
			ext.execute_with(|| System::set_block_number(1));
			ext
		}
		fn block() -> u64 {
			System::block_number()
		}
		fn set_block(n: u64) {
			System::set_block_number(n)
		}
		fn free(who: u64) -> Balance {
			Balances::free_balance(acc(who)).into()
		}
		fn ledger(who: u64) -> (Balance, Balance) {
			let data = System::account(acc(who)).data;
			(data.frozen.into(), data.reserved.into())
		}
		fn total_issuance() -> Balance {
			<Balances as frame_support::traits::fungible::Inspect<_>>::total_issuance().into()
		}
		fn reset_events() {
			System::reset_events()
		}
		// Only an account that exists can vote, or be locked for any other reason.
		#[allow(deprecated)] // `LockableCurrency` is what conviction voting still uses.
		fn lock(who: u64, amount: Balance) {
			use frame_support::traits::{LockableCurrency, WithdrawReasons};
			if System::account_exists(&acc(who)) {
				let amount = amount.try_into().expect("lock amounts fit every runtime");
				Balances::set_lock(*b"democrac", &acc(who), amount, WithdrawReasons::all());
			}
		}
		#[allow(deprecated)]
		fn unlock(who: u64) {
			use frame_support::traits::LockableCurrency;
			Balances::remove_lock(*b"democrac", &acc(who));
		}
		fn transfer(from: u64, to: u64, amount: Balance) {
			use frame_support::traits::{fungible::Mutate, tokens::Preservation};
			let amount = amount.try_into().expect("spend amounts fit every runtime");
			let _ = frame_support::storage::with_storage_layer(|| {
				<Balances as Mutate<_>>::transfer(
					&acc(from),
					&acc(to),
					amount,
					Preservation::Expendable,
				)
			});
		}
	};
}

pub mod v2 {
	use super::*;
	use frame_support::{
		derive_impl,
		traits::{ConstU128, ConstU32, ConstU64},
		BoundedVec, PalletId,
	};
	use pallet_escrow::Milestone;
	use sp_runtime::{traits::IdentityLookup, BuildStorage, DispatchError};

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
		type AccountId = u128;
		type Lookup = IdentityLookup<u128>;
		type AccountData = pallet_balances::AccountData<u128>;
	}

	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for Runtime {
		type AccountStore = System;
		type Balance = u128;
		type ExistentialDeposit = ConstU128<{ EXISTENTIAL_DEPOSIT as u128 }>;
	}

	pub const MIN_MILESTONE: u128 = 10;
	pub const DEPOSIT: u128 = 5;
	pub const MAX_MILESTONES: u32 = 8;
	pub const CHALLENGE_PERIOD: u64 = 3;

	frame_support::parameter_types! {
		pub const EscrowPalletId: PalletId = PalletId(*b"py/escrw");
	}

	impl pallet_escrow::Config for Runtime {
		type RuntimeEvent = RuntimeEvent;
		type Currency = Balances;
		type RuntimeHoldReason = RuntimeHoldReason;
		type PalletId = EscrowPalletId;
		type MaxMilestones = ConstU32<MAX_MILESTONES>;
		type MinMilestone = ConstU128<MIN_MILESTONE>;
		type EscrowDeposit = ConstU128<DEPOSIT>;
		type ChallengePeriod = ConstU64<CHALLENGE_PERIOD>;
		type WeightInfo = ();
	}

	fn acc(who: u64) -> u128 {
		u128::from(who)
	}

	fn party(who: u128) -> u64 {
		u64::try_from(who).expect("parties are harness accounts")
	}

	/// Run the migration from the hold-based layout.
	pub fn migrate() {
		use frame_support::traits::OnRuntimeUpgrade;
		pallet_escrow::migrations::v1::MigrateToV1::<Runtime>::on_runtime_upgrade();
	}

	/// What is still on hold for `who` under the reasons the hold-based layout used.
	pub fn legacy_holds(who: u64) -> Balance {
		use frame_support::traits::fungible::InspectHold;
		[pallet_escrow::HoldReason::Escrow, pallet_escrow::HoldReason::StorageDeposit]
			.into_iter()
			.map(|reason| Balances::balance_on_hold(&reason.into(), &acc(who)))
			.sum()
	}

	fn payer_escrows(who: u64) -> impl Iterator<Item = (u32, pallet_escrow::EscrowInfo<Runtime>)> {
		pallet_escrow::Escrows::<Runtime>::iter().filter(move |(_, e)| e.payer == acc(who))
	}

	/// The product pallet: claims, a deadline per milestone, funds in an account per escrow.
	pub struct V2;

	impl Target for V2 {
		const NAME: &'static str = "pallet-escrow";
		const PARAMS: SpecParams = SpecParams {
			min_milestone: MIN_MILESTONE,
			deposit: DEPOSIT,
			max_milestones: MAX_MILESTONES as usize,
			existential_deposit: EXISTENTIAL_DEPOSIT as Balance,
			max_balance: u128::MAX,
			custody: Custody::Account,
			claims: true,
			challenge_period: CHALLENGE_PERIOD,
		};

		ledger!();

		fn create(
			payer: u64,
			beneficiary: u64,
			arbiter: Option<u64>,
			milestones: Vec<(Balance, u64)>,
			_deadline: u64,
		) -> DispatchResult {
			// An oversized vector never decodes into a `BoundedVec` call argument, so the call
			// could not reach the pallet at all.
			let milestones: BoundedVec<_, ConstU32<MAX_MILESTONES>> = milestones
				.into_iter()
				.map(|(amount, deadline)| Milestone { amount, deadline })
				.collect::<Vec<_>>()
				.try_into()
				.map_err(|_| DispatchError::Other(UNDECODABLE))?;
			Escrow::create(
				RuntimeOrigin::signed(acc(payer)),
				acc(beneficiary),
				arbiter.map(acc),
				milestones,
			)
		}
		fn release(who: u64, id: u32) -> DispatchResult {
			Escrow::release(RuntimeOrigin::signed(acc(who)), id)
		}
		fn refund(who: u64, id: u32) -> DispatchResult {
			Escrow::refund(RuntimeOrigin::signed(acc(who)), id)
		}
		fn cancel(who: u64, id: u32) -> DispatchResult {
			Escrow::cancel(RuntimeOrigin::signed(acc(who)), id)
		}
		fn submit(who: u64, id: u32) -> DispatchResult {
			Escrow::submit(RuntimeOrigin::signed(acc(who)), id)
		}
		fn dispute(who: u64, id: u32) -> DispatchResult {
			Escrow::dispute(RuntimeOrigin::signed(acc(who)), id)
		}
		fn claim(who: u64, id: u32) -> DispatchResult {
			Escrow::claim(RuntimeOrigin::signed(acc(who)), id)
		}
		fn resolve(who: u64, id: u32, to_beneficiary: Balance) -> DispatchResult {
			Escrow::resolve(RuntimeOrigin::signed(acc(who)), id, to_beneficiary)
		}
		// What is in the payer's escrow accounts, less the deposits: the ledger's view, not
		// the pallet's bookkeeping.
		fn escrowed(who: u64) -> Balance {
			payer_escrows(who)
				.map(|(id, e)| {
					Balances::free_balance(Escrow::account(id)).saturating_sub(e.deposit)
				})
				.sum()
		}
		fn deposits(who: u64) -> Balance {
			payer_escrows(who).map(|(_, e)| e.deposit).sum()
		}
		fn next_id() -> u32 {
			pallet_escrow::NextEscrowId::<Runtime>::get()
		}
		fn escrows() -> BTreeMap<u32, Record> {
			pallet_escrow::Escrows::<Runtime>::iter()
				.map(|(id, e)| {
					let record = Record {
						payer: party(e.payer),
						beneficiary: party(e.beneficiary),
						arbiter: e.arbiter.map(party),
						milestones: e.milestones.iter().map(|m| (m.amount, m.deadline)).collect(),
						released: e.released,
						remaining: e.remaining,
						deposit: e.deposit,
						claim: e
							.claim
							.map(|c| Claim { submitted: c.submitted, disputed: c.disputed }),
					};
					(id, record)
				})
				.collect()
		}
		fn events() -> Option<Vec<SpecEvent>> {
			use pallet_escrow::Event as E;
			let events = System::events().into_iter().filter_map(|r| match r.event {
				RuntimeEvent::Escrow(e) => Some(match e {
					E::Created { id, payer, beneficiary, total } => SpecEvent::Created {
						id,
						payer: party(payer),
						beneficiary: party(beneficiary),
						total,
					},
					E::MilestoneReleased { id, index, amount } => {
						SpecEvent::MilestoneReleased { id, index, amount }
					},
					E::Completed { id } => SpecEvent::Completed { id },
					E::Refunded { id, amount } => SpecEvent::Refunded { id, amount },
					E::Cancelled { id, amount } => SpecEvent::Cancelled { id, amount },
					E::Submitted { id, index } => SpecEvent::Submitted { id, index },
					E::Disputed { id, index } => SpecEvent::Disputed { id, index },
					E::Resolved { id, index, to_beneficiary, to_payer } => {
						SpecEvent::Resolved { id, index, to_beneficiary, to_payer }
					},
					E::__Ignore(..) => unreachable!("never constructed"),
				}),
				_ => None,
			});
			Some(events.collect())
		}
		fn try_state() -> Result<(), String> {
			Escrow::do_try_state().map_err(|e| format!("{e:?}"))?;
			for id in 0..Self::next_id() {
				if !pallet_escrow::Escrows::<Runtime>::contains_key(id)
					&& Balances::free_balance(Escrow::account(id)) != 0
				{
					return Err(format!("escrow {id} is closed but its account is not empty"));
				}
			}
			Ok(())
		}
	}
}

pub mod v1 {
	use super::*;
	use frame_support::{
		derive_impl,
		traits::{fungible::InspectHold, ConstU128, ConstU32},
		BoundedVec,
	};
	use sp_runtime::{traits::IdentityLookup, BuildStorage, DispatchError};

	type Block = frame_system::mocking::MockBlock<Runtime>;

	frame_support::construct_runtime!(
		pub enum Runtime {
			System: frame_system,
			Balances: pallet_balances,
			Escrow: pallet_escrow_v1,
		}
	);

	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for Runtime {
		type Block = Block;
		type AccountId = u128;
		type Lookup = IdentityLookup<u128>;
		type AccountData = pallet_balances::AccountData<u128>;
	}

	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for Runtime {
		type AccountStore = System;
		type Balance = u128;
		type ExistentialDeposit = ConstU128<{ EXISTENTIAL_DEPOSIT as u128 }>;
	}

	pub const MIN_MILESTONE: u128 = 10;
	pub const DEPOSIT: u128 = 5;
	pub const MAX_MILESTONES: u32 = 8;

	impl pallet_escrow_v1::Config for Runtime {
		type RuntimeEvent = RuntimeEvent;
		type Currency = Balances;
		type RuntimeHoldReason = RuntimeHoldReason;
		type MaxMilestones = ConstU32<MAX_MILESTONES>;
		type MinMilestone = ConstU128<MIN_MILESTONE>;
		type EscrowDeposit = ConstU128<DEPOSIT>;
		type WeightInfo = ();
	}

	fn acc(who: u64) -> u128 {
		u128::from(who)
	}

	fn party(who: u128) -> u64 {
		u64::try_from(who).expect("parties are harness accounts")
	}

	/// The hold-based pallet, as of the ESC-11 fix.
	pub struct V1;

	impl Target for V1 {
		const NAME: &'static str = "pallet-escrow-v1";
		const PARAMS: SpecParams = SpecParams {
			min_milestone: MIN_MILESTONE,
			deposit: DEPOSIT,
			max_milestones: MAX_MILESTONES as usize,
			existential_deposit: EXISTENTIAL_DEPOSIT as Balance,
			max_balance: u128::MAX,
			custody: Custody::Hold,
			claims: false,
			challenge_period: 0,
		};

		ledger!();

		fn create(
			payer: u64,
			beneficiary: u64,
			arbiter: Option<u64>,
			milestones: Vec<(Balance, u64)>,
			deadline: u64,
		) -> DispatchResult {
			let milestones: BoundedVec<u128, ConstU32<MAX_MILESTONES>> = milestones
				.into_iter()
				.map(|(amount, _)| amount)
				.collect::<Vec<_>>()
				.try_into()
				.map_err(|_| DispatchError::Other(UNDECODABLE))?;
			Escrow::create(
				RuntimeOrigin::signed(acc(payer)),
				acc(beneficiary),
				arbiter.map(acc),
				milestones,
				deadline,
			)
		}
		fn release(who: u64, id: u32) -> DispatchResult {
			Escrow::release(RuntimeOrigin::signed(acc(who)), id)
		}
		fn refund(who: u64, id: u32) -> DispatchResult {
			Escrow::refund(RuntimeOrigin::signed(acc(who)), id)
		}
		fn cancel(who: u64, id: u32) -> DispatchResult {
			Escrow::cancel(RuntimeOrigin::signed(acc(who)), id)
		}
		fn escrowed(who: u64) -> Balance {
			Balances::balance_on_hold(&pallet_escrow_v1::HoldReason::Escrow.into(), &acc(who))
		}
		fn deposits(who: u64) -> Balance {
			Balances::balance_on_hold(
				&pallet_escrow_v1::HoldReason::StorageDeposit.into(),
				&acc(who),
			)
		}
		fn next_id() -> u32 {
			pallet_escrow_v1::NextEscrowId::<Runtime>::get()
		}
		fn escrows() -> BTreeMap<u32, Record> {
			pallet_escrow_v1::Escrows::<Runtime>::iter()
				.map(|(id, e)| {
					let record = Record {
						payer: party(e.payer),
						beneficiary: party(e.beneficiary),
						arbiter: e.arbiter.map(party),
						milestones: e.milestones.iter().map(|m| (*m, e.deadline)).collect(),
						released: e.released,
						remaining: e.remaining,
						deposit: e.deposit,
						claim: None,
					};
					(id, record)
				})
				.collect()
		}
		fn events() -> Option<Vec<SpecEvent>> {
			use pallet_escrow_v1::Event as E;
			let events = System::events().into_iter().filter_map(|r| match r.event {
				RuntimeEvent::Escrow(e) => Some(match e {
					E::Created { id, payer, beneficiary, total } => SpecEvent::Created {
						id,
						payer: party(payer),
						beneficiary: party(beneficiary),
						total,
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
		fn try_state() -> Result<(), String> {
			Escrow::do_try_state().map_err(|e| format!("{e:?}"))
		}
	}
}

pub mod v0 {
	use super::*;
	use frame_support::{derive_impl, traits::fungible::InspectHold};
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

	fn acc(who: u64) -> u64 {
		who
	}

	fn amount(value: Balance) -> u64 {
		u64::try_from(value).expect("v0 amounts are generated within u64")
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
			max_milestones: v1::MAX_MILESTONES as usize,
			existential_deposit: EXISTENTIAL_DEPOSIT as Balance,
			max_balance: u64::MAX as Balance,
			custody: Custody::Hold,
			claims: false,
			challenge_period: 0,
		};

		ledger!();

		fn create(
			payer: u64,
			beneficiary: u64,
			arbiter: Option<u64>,
			milestones: Vec<(Balance, u64)>,
			deadline: u64,
		) -> DispatchResult {
			let milestones = milestones.into_iter().map(|(m, _)| amount(m)).collect();
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
		fn escrowed(who: u64) -> Balance {
			Balances::balance_on_hold(&pallet_escrow_v0::HoldReason::Escrow.into(), &who).into()
		}
		fn deposits(_who: u64) -> Balance {
			0
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
						milestones: e
							.milestones
							.iter()
							.map(|m| ((*m).into(), e.deadline))
							.collect(),
						released: e.released,
						remaining: e.remaining.into(),
						deposit: 0,
						claim: None,
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
	}
}
