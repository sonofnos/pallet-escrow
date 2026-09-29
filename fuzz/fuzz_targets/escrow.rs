//! Coverage-guided search for spec violations in the hardened pallet. Any crash is a bug.
#![no_main]

use escrow_harness::{
	runtimes::fixed::Fixed,
	spec::{self, Action},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|actions: Vec<Action>| {
	if let Err(violation) = spec::run::<Fixed>(&actions) {
		panic!("{violation}");
	}
});
