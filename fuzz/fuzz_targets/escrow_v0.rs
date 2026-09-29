//! The same search against the audited first draft. Expected to crash within seconds.
#![no_main]

use escrow_harness::{actions_from_bytes, runtimes::v0::V0, spec};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	if let Err(violation) = spec::run::<V0>(&actions_from_bytes(data)) {
		panic!("{violation}");
	}
});
