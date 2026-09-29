//! Coverage-guided search for spec violations in the hardened pallet. Any crash is a bug.
#![no_main]

use escrow_harness::{actions_from_bytes, runtimes::fixed::Fixed, spec};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	if let Err(violation) = spec::run::<Fixed>(&actions_from_bytes(data)) {
		panic!("{violation}");
	}
});
