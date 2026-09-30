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
	fn submit() -> Weight;
	fn dispute() -> Weight;
	fn claim() -> Weight;
	fn resolve() -> Weight;
}

/// Weights for a runtime using this pallet, scaled by the runtime's own DB weights.
pub struct SubstrateWeight<T>(PhantomData<T>);

// Reads/writes per call:
// create:  NextEscrowId (r/w), Escrows (w), payer + escrow accounts (r/w)
// release / claim: Escrows (r/w), escrow + beneficiary accounts (r/w), payer account on close
// refund / cancel: Escrows (r/w), escrow + payer accounts (r/w)
// submit / dispute: Escrows (r/w)
// resolve: Escrows (r/w), escrow, beneficiary and payer accounts (r/w)
impl<T: frame_system::Config> WeightInfo for SubstrateWeight<T> {
	fn create(m: u32) -> Weight {
		Weight::from_parts(80_000_000, 4_000)
			.saturating_add(Weight::from_parts(250_000, 24).saturating_mul(m.into()))
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
	fn release() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
	fn refund() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(T::DbWeight::get().reads(3))
			.saturating_add(T::DbWeight::get().writes(3))
	}
	fn cancel() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(T::DbWeight::get().reads(3))
			.saturating_add(T::DbWeight::get().writes(3))
	}
	fn submit() -> Weight {
		Weight::from_parts(30_000_000, 3_000)
			.saturating_add(T::DbWeight::get().reads(1))
			.saturating_add(T::DbWeight::get().writes(1))
	}
	fn dispute() -> Weight {
		Weight::from_parts(30_000_000, 3_000)
			.saturating_add(T::DbWeight::get().reads(1))
			.saturating_add(T::DbWeight::get().writes(1))
	}
	fn claim() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
	fn resolve() -> Weight {
		Weight::from_parts(110_000_000, 8_000)
			.saturating_add(T::DbWeight::get().reads(4))
			.saturating_add(T::DbWeight::get().writes(4))
	}
}

impl WeightInfo for () {
	fn create(m: u32) -> Weight {
		Weight::from_parts(80_000_000, 4_000)
			.saturating_add(Weight::from_parts(250_000, 24).saturating_mul(m.into()))
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
	fn release() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
	fn refund() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(RocksDbWeight::get().reads(3))
			.saturating_add(RocksDbWeight::get().writes(3))
	}
	fn cancel() -> Weight {
		Weight::from_parts(70_000_000, 4_000)
			.saturating_add(RocksDbWeight::get().reads(3))
			.saturating_add(RocksDbWeight::get().writes(3))
	}
	fn submit() -> Weight {
		Weight::from_parts(30_000_000, 3_000)
			.saturating_add(RocksDbWeight::get().reads(1))
			.saturating_add(RocksDbWeight::get().writes(1))
	}
	fn dispute() -> Weight {
		Weight::from_parts(30_000_000, 3_000)
			.saturating_add(RocksDbWeight::get().reads(1))
			.saturating_add(RocksDbWeight::get().writes(1))
	}
	fn claim() -> Weight {
		Weight::from_parts(90_000_000, 6_000)
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
	fn resolve() -> Weight {
		Weight::from_parts(110_000_000, 8_000)
			.saturating_add(RocksDbWeight::get().reads(4))
			.saturating_add(RocksDbWeight::get().writes(4))
	}
}
