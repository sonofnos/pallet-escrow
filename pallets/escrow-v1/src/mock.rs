use crate as pallet_escrow_v1;
use frame_support::{
	derive_impl,
	traits::{ConstU32, ConstU64},
};
use sp_runtime::BuildStorage;

type Block = frame_system::mocking::MockBlock<Test>;

frame_support::construct_runtime!(
	pub enum Test {
		System: frame_system,
		Balances: pallet_balances,
		Escrow: pallet_escrow_v1,
	}
);

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
	type Block = Block;
	type AccountData = pallet_balances::AccountData<u64>;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
	type AccountStore = System;
}

pub const MIN_MILESTONE: u64 = 10;
pub const DEPOSIT: u64 = 5;
pub const MAX_MILESTONES: u32 = 8;

impl pallet_escrow_v1::Config for Test {
	type RuntimeEvent = RuntimeEvent;
	type Currency = Balances;
	type RuntimeHoldReason = RuntimeHoldReason;
	type MaxMilestones = ConstU32<MAX_MILESTONES>;
	type MinMilestone = ConstU64<MIN_MILESTONE>;
	type EscrowDeposit = ConstU64<DEPOSIT>;
	type WeightInfo = ();
}

pub const PAYER: u64 = 1;
pub const BENEFICIARY: u64 = 2;
pub const ARBITER: u64 = 3;
pub const STRANGER: u64 = 4;
pub const START_BALANCE: u64 = 1_000;

pub fn new_test_ext() -> sp_io::TestExternalities {
	let mut storage = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
	pallet_balances::GenesisConfig::<Test> {
		balances: vec![
			(PAYER, START_BALANCE),
			(BENEFICIARY, START_BALANCE),
			(ARBITER, START_BALANCE),
			(STRANGER, START_BALANCE),
		],
		..Default::default()
	}
	.assimilate_storage(&mut storage)
	.unwrap();
	let mut ext: sp_io::TestExternalities = storage.into();
	ext.execute_with(|| System::set_block_number(1));
	ext
}
