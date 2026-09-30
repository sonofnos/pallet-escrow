//! Print every class of spec violation the harness finds, with a shrunk reproduction.
//!
//! cargo run --release -p escrow-harness --example hunt -- v0 20000    # or v1, v2

use escrow_harness::{
	hunt, quiet_panics,
	runtimes::{v0::V0, v1::V1, v2::V2},
	Finding,
};

fn main() {
	let mut args = std::env::args().skip(1);
	let target = args.next().unwrap_or_else(|| "v0".into());
	let seeds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);
	quiet_panics();

	let findings: Vec<Finding> = match target.as_str() {
		"v0" => hunt::<V0>(seeds),
		"v1" => hunt::<V1>(seeds),
		"v2" => hunt::<V2>(seeds),
		other => panic!("unknown target {other}, use v0, v1 or v2"),
	};

	println!("{target}: {seeds} seeded sequences, {} violation classes", findings.len());
	println!("calls (including shrinking):\n{}\n", escrow_harness::spec::coverage());
	for f in &findings {
		println!("## {}\nfirst seed {}, {} sequences hit it", f.label, f.seed, f.hits);
		println!("shrunk to {} calls:", f.reproduction.len());
		for a in &f.reproduction {
			println!("  {a:?}");
		}
		println!("{}\n", f.violation);
	}
}
