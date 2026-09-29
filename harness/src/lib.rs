//! Spec-oracle harness for `pallet-escrow` and its audited first draft.
//!
//! [`spec::run`] drives one call sequence and checks it against the spec. [`hunt`] runs many
//! seeded sequences on stable Rust (for CI); the `fuzz/` crate feeds the same driver from
//! libFuzzer for coverage-guided search.

pub mod runtimes;
pub mod spec;

use arbitrary::{Arbitrary, Unstructured};
use spec::{Action, Target, Violation};
use std::collections::BTreeMap;

/// SplitMix64: tiny, deterministic, good enough to generate fuzz bytes.
struct SplitMix(u64);

impl SplitMix {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^ (z >> 31)
	}
}

/// Decode actions until the bytes run out, up to [`spec::MAX_ACTIONS`].
///
/// `Vec::<Action>::arbitrary` continues on a coin flip per element, which gives sequences of
/// about two calls on random input: far too short to reach a released-then-cancelled escrow.
pub fn actions_from_bytes(data: &[u8]) -> Vec<Action> {
	let mut u = Unstructured::new(data);
	let mut actions = Vec::new();
	while actions.len() < spec::MAX_ACTIONS {
		match Action::arbitrary(&mut u) {
			Ok(a) if !u.is_empty() => actions.push(a),
			_ => break,
		}
	}
	actions
}

/// The call sequence for a seed. Same seed, same sequence, on every machine.
pub fn actions_for_seed(seed: u64) -> Vec<Action> {
	let mut rng = SplitMix(seed);
	let len = 8 + (rng.next() % 41) as usize;
	let bytes: Vec<u8> = (0..len * 3).flat_map(|_| rng.next().to_le_bytes()).collect();
	let mut actions = actions_from_bytes(&bytes);
	actions.truncate(len);
	actions
}

/// The first, shrunk, reproduction of one class of bug.
#[derive(Debug)]
pub struct Finding {
	pub label: String,
	pub seed: u64,
	pub hits: usize,
	pub reproduction: Vec<Action>,
	pub violation: Violation,
}

/// Run `seeds` sequences and group violations by label.
pub fn hunt<T: Target>(seeds: u64) -> Vec<Finding> {
	let mut found: BTreeMap<String, Finding> = BTreeMap::new();
	for seed in 0..seeds {
		let actions = actions_for_seed(seed);
		if let Err(v) = spec::run::<T>(&actions) {
			let label = v.label();
			match found.get_mut(&label) {
				Some(f) => f.hits += 1,
				None => {
					let (reproduction, violation) = spec::shrink::<T>(&actions, &v);
					found.insert(
						label.clone(),
						Finding { label, seed, hits: 1, reproduction, violation },
					);
				},
			}
		}
	}
	found.into_values().collect()
}

/// Silence the default panic printout; the harness reports panics itself.
pub fn quiet_panics() {
	std::panic::set_hook(Box::new(|_| {}));
}
