//! The same search against the audited first draft. Expected to crash within seconds.
#![no_main]

use escrow_harness::{
	runtimes::v0::V0,
	spec::{self, Action},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|actions: Vec<Action>| {
	if let Err(violation) = spec::run::<V0>(&actions) {
		panic!("{violation}");
	}
});
