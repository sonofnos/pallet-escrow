//! A minimal runtime to benchmark `pallet-escrow` in, with `frame-omni-bencher`.
//!
//! It is not a chain: no consensus, no fees, nothing but what the benchmarks run against. What
//! it does fix is what a weight depends on: `AccountId32` accounts, `u128` balances, RocksDB
//! weights and the escrow configuration a production runtime would plausibly use.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
include!(concat!(env!("OUT_DIR"), "/wasm_binary.rs"));

extern crate alloc;

use alloc::{vec, vec::Vec};
use frame_support::{
	derive_impl,
	genesis_builder_helper::{build_state, get_preset},
	parameter_types,
	traits::{ConstU128, ConstU32},
	weights::constants::RocksDbWeight,
	PalletId,
};
use sp_api::impl_runtime_apis;
use sp_core::OpaqueMetadata;
use sp_genesis_builder::PresetId;
use sp_runtime::{
	generic,
	traits::{BlakeTwo256, Block as BlockT, IdentifyAccount, Verify},
	ApplyExtrinsicResult, MultiAddress, MultiSignature,
};
use sp_version::RuntimeVersion;

#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
	spec_name: alloc::borrow::Cow::Borrowed("escrow-bench"),
	impl_name: alloc::borrow::Cow::Borrowed("escrow-bench"),
	authoring_version: 1,
	spec_version: 1,
	impl_version: 1,
	apis: RUNTIME_API_VERSIONS,
	transaction_version: 1,
	system_version: 1,
};

pub type Signature = MultiSignature;
pub type AccountId = <<Signature as Verify>::Signer as IdentifyAccount>::AccountId;
pub type Balance = u128;
pub type BlockNumber = u32;

pub const UNIT: Balance = 1_000_000_000_000;
pub const EXISTENTIAL_DEPOSIT: Balance = UNIT / 1_000;

type Header = generic::Header<BlockNumber, BlakeTwo256>;
type TxExtension = (
	frame_system::CheckNonZeroSender<Runtime>,
	frame_system::CheckNonce<Runtime>,
	frame_system::CheckWeight<Runtime>,
);
type UncheckedExtrinsic =
	generic::UncheckedExtrinsic<MultiAddress<AccountId, ()>, RuntimeCall, Signature, TxExtension>;
pub type Block = generic::Block<Header, UncheckedExtrinsic>;

type Executive = frame_executive::Executive<
	Runtime,
	Block,
	frame_system::ChainContext<Runtime>,
	Runtime,
	AllPalletsWithSystem,
>;

#[frame_support::runtime]
mod runtime {
	#[runtime::runtime]
	#[runtime::derive(
		RuntimeCall,
		RuntimeEvent,
		RuntimeError,
		RuntimeOrigin,
		RuntimeFreezeReason,
		RuntimeHoldReason,
		RuntimeSlashReason,
		RuntimeLockId,
		RuntimeTask,
		RuntimeViewFunction
	)]
	pub struct Runtime;

	#[runtime::pallet_index(0)]
	pub type System = frame_system;

	#[runtime::pallet_index(1)]
	pub type Balances = pallet_balances;

	#[runtime::pallet_index(2)]
	pub type Escrow = pallet_escrow;
}

parameter_types! {
	pub const Version: RuntimeVersion = VERSION;
	pub const EscrowPalletId: PalletId = PalletId(*b"py/escrw");
}

#[derive_impl(frame_system::config_preludes::SolochainDefaultConfig)]
impl frame_system::Config for Runtime {
	type Block = Block;
	type AccountId = AccountId;
	type Lookup = sp_runtime::traits::AccountIdLookup<AccountId, ()>;
	type AccountData = pallet_balances::AccountData<Balance>;
	type Version = Version;
	type DbWeight = RocksDbWeight;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Runtime {
	type Balance = Balance;
	type ExistentialDeposit = ConstU128<EXISTENTIAL_DEPOSIT>;
	type AccountStore = System;
	type WeightInfo = ();
}

impl pallet_escrow::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type Currency = Balances;
	type RuntimeHoldReason = RuntimeHoldReason;
	type PalletId = EscrowPalletId;
	type MaxMilestones = ConstU32<16>;
	type MinMilestone = ConstU128<{ UNIT / 100 }>;
	type EscrowDeposit = ConstU128<{ UNIT / 10 }>;
	/// A day of six-second blocks.
	type ChallengePeriod = ConstU32<14_400>;
	type WeightInfo = pallet_escrow::weights::SubstrateWeight<Runtime>;
}

#[cfg(feature = "runtime-benchmarks")]
frame_benchmarking::define_benchmarks!([pallet_escrow, Escrow]);

impl_runtime_apis! {
	impl sp_api::Core<Block> for Runtime {
		fn version() -> RuntimeVersion {
			VERSION
		}

		fn execute_block(block: <Block as BlockT>::LazyBlock) {
			Executive::execute_block(block);
		}

		fn initialize_block(header: &<Block as BlockT>::Header) -> sp_runtime::ExtrinsicInclusionMode {
			Executive::initialize_block(header)
		}
	}

	impl sp_api::Metadata<Block> for Runtime {
		fn metadata() -> OpaqueMetadata {
			OpaqueMetadata::new(Runtime::metadata().into())
		}

		fn metadata_at_version(version: u32) -> Option<OpaqueMetadata> {
			Runtime::metadata_at_version(version)
		}

		fn metadata_versions() -> Vec<u32> {
			Runtime::metadata_versions()
		}
	}

	impl sp_block_builder::BlockBuilder<Block> for Runtime {
		fn apply_extrinsic(extrinsic: <Block as BlockT>::Extrinsic) -> ApplyExtrinsicResult {
			Executive::apply_extrinsic(extrinsic)
		}

		fn finalize_block() -> <Block as BlockT>::Header {
			Executive::finalize_block()
		}

		fn inherent_extrinsics(data: sp_inherents::InherentData) -> Vec<<Block as BlockT>::Extrinsic> {
			data.create_extrinsics()
		}

		fn check_inherents(
			block: <Block as BlockT>::LazyBlock,
			data: sp_inherents::InherentData,
		) -> sp_inherents::CheckInherentsResult {
			data.check_extrinsics(&block)
		}
	}

	impl sp_genesis_builder::GenesisBuilder<Block> for Runtime {
		fn build_state(config: Vec<u8>) -> sp_genesis_builder::Result {
			build_state::<RuntimeGenesisConfig>(config)
		}

		// The benchmarks fund their own accounts, so the development preset is the default
		// genesis.
		fn get_preset(id: &Option<PresetId>) -> Option<Vec<u8>> {
			get_preset::<RuntimeGenesisConfig>(id, |id| {
				(AsRef::<str>::as_ref(id) == sp_genesis_builder::DEV_RUNTIME_PRESET)
					.then(|| b"{}".to_vec())
			})
		}

		fn preset_names() -> Vec<PresetId> {
			vec![PresetId::from(sp_genesis_builder::DEV_RUNTIME_PRESET)]
		}
	}

	#[cfg(feature = "runtime-benchmarks")]
	impl frame_benchmarking::Benchmark<Block> for Runtime {
		fn benchmark_metadata(extra: bool) -> (
			Vec<frame_benchmarking::BenchmarkList>,
			Vec<frame_support::traits::StorageInfo>,
		) {
			use frame_benchmarking::BenchmarkList;
			use frame_support::traits::StorageInfoTrait;

			let mut list = Vec::<BenchmarkList>::new();
			list_benchmarks!(list, extra);
			(list, AllPalletsWithSystem::storage_info())
		}

		#[allow(non_local_definitions)]
		fn dispatch_benchmark(
			config: frame_benchmarking::BenchmarkConfig
		) -> Result<Vec<frame_benchmarking::BenchmarkBatch>, alloc::string::String> {
			use frame_benchmarking::BenchmarkBatch;
			use frame_support::traits::WhitelistedStorageKeys;
			use sp_storage::TrackedStorageKey;

			let whitelist: Vec<TrackedStorageKey> = AllPalletsWithSystem::whitelisted_storage_keys();
			let mut batches = Vec::<BenchmarkBatch>::new();
			let params = (&config, &whitelist);
			add_benchmarks!(params, batches);
			Ok(batches)
		}
	}
}
