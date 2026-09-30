//! Weights for `pallet_escrow`.
//!
//! These are hand-set upper bounds, not benchmark output: the storage reads/writes are counted
//! from the code and the ref-time figures are deliberately generous. Regenerate with
//! `frame-omni-bencher` against a real runtime before production use (the benchmarks in
//! `benchmarking.rs` already cover every call, including the `create` milestone component).

#![allow(unused_parens, unused_imports)]

use core::marker::PhantomData;
use frame_support::{
	traits::Get,
	weights::{constants::RocksDbWeight, Weight},
};

pub trait WeightInfo {
	fn create(m: u32) -> Weight;
	fn release() -> Weight;
	fn refund() -> Weight;
	fn cancel() -> Weight;
}

/// Weights for a runtime using this pallet, scaled by the runtime's own DB weights.
pub struct SubstrateWeight<T>(PhantomData<T>);

// Reads/writes per call:
// create:  NextEscrowId (r/w), EscrowCount (r/w), Escrows (w), payer account + holds (r/w x2)
// release: Escrows (r/w), payer + beneficiary accounts, payer holds; EscrowCount (r/w) on completion
// refund / cancel: Escrows (r/w), EscrowCount (r/w), payer account + holds (r/w x2)
impl<T: frame_system::Config> WeightInfo for SubstrateWeight<T> {
	fn create(m: u32) -> Weight {
		Weight::from_parts(80_000_000, 4_000)
			.saturating_add(Weight::from_parts(250_000, 16).saturating_mul(m.into()))
			.saturating_add(T::DbWeight::get().reads(5))
			.saturating_add(T::DbWeight::get().writes(5))
	}
	fn release() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(T::DbWeight::get().reads(5))
			.saturating_add(T::DbWeight::get().writes(6))
	}
	fn refund() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
	fn cancel() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
}

impl WeightInfo for () {
	fn create(m: u32) -> Weight {
		Weight::from_parts(80_000_000, 4_000)
			.saturating_add(Weight::from_parts(250_000, 16).saturating_mul(m.into()))
			.saturating_add(RocksDbWeight::get().reads(5))
			.saturating_add(RocksDbWeight::get().writes(5))
	}
	fn release() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(RocksDbWeight::get().reads(5))
			.saturating_add(RocksDbWeight::get().writes(6))
	}
	fn refund() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
	fn cancel() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
}
