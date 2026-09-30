//! The storage migration, checked on state the hold-based pallet actually produced.
//!
//! Each case runs a random sequence against `pallet-escrow-v1`, moves the resulting storage into
//! a runtime with the current pallet and migrates it. Nothing observable may change: the same
//! escrows, and every account's free balance, escrowed funds and deposits. Then another random
//! sequence runs against the migrated escrows, held to the full spec, claims included.

use escrow_harness::{
	actions_for_seed, quiet_panics,
	runtimes::{
		v1::V1,
		v2::{self, V2},
		ACCOUNTS,
	},
	spec::{self, Balance, Record, Target},
};
use sp_io::TestExternalities;
use std::collections::BTreeMap;

fn seeds(default: u64) -> u64 {
	std::env::var("HARNESS_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

type Observed = (BTreeMap<u32, Record>, Vec<(Balance, Balance, Balance)>, Balance, u32);

fn observe<T: Target>() -> Observed {
	let balances = (1..=ACCOUNTS).map(|w| (T::free(w), T::escrowed(w), T::deposits(w))).collect();
	(T::escrows(), balances, T::total_issuance(), T::next_id())
}

#[test]
fn migrated_escrows_keep_their_state_and_follow_the_spec() {
	quiet_panics();
	let mut migrated = 0;
	for seed in 0..seeds(4_000) / 4 {
		let mut chain = V1::new_ext();
		spec::run_in::<V1>(&mut chain, &actions_for_seed(seed)).expect("v1 follows the spec");
		let before = chain.execute_with(observe::<V1>);
		chain.commit_all().expect("changes commit");
		let (raw, root) = chain.into_raw_snapshot();
		let mut chain = TestExternalities::from_raw_snapshot(raw, root, Default::default());

		chain.execute_with(|| {
			v2::migrate();
			assert_eq!(
				observe::<V2>(),
				before,
				"seed {seed}: migration changed what is observable"
			);
			for who in 1..=ACCOUNTS {
				assert_eq!(v2::legacy_holds(who), 0, "seed {seed}: hold left on account {who}");
			}
			V2::try_state().unwrap_or_else(|e| panic!("seed {seed}: {e}"));
		});
		migrated += before.0.len();

		let after = actions_for_seed(seed + (1 << 32));
		if let Err(violation) = spec::run_in::<V2>(&mut chain, &after) {
			panic!("seed {seed}, after migration: {violation}");
		}
	}
	eprintln!("{migrated} live escrows migrated");
	assert!(migrated > 0, "no sequence left a live escrow to migrate");
}
