use escrow_harness::{
	hunt, quiet_panics,
	runtimes::{fixed::Fixed, v0::V0},
};

fn seeds(default: u64) -> u64 {
	std::env::var("HARNESS_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

#[test]
fn hardened_pallet_never_violates_the_spec() {
	quiet_panics();
	let findings = hunt::<Fixed>(seeds(4_000));
	for f in &findings {
		eprintln!(
			"{} (seed {}, {} hits)\n  {}\n  repro: {:?}",
			f.label, f.seed, f.hits, f.violation, f.reproduction
		);
	}
	assert!(findings.is_empty(), "{} violation classes in pallet-escrow", findings.len());
}

/// The audit's findings, each rediscovered by the harness without being told where to look.
#[test]
fn harness_rediscovers_the_v0_audit_findings() {
	quiet_panics();
	let findings = hunt::<V0>(seeds(4_000));
	let labels: Vec<&str> = findings.iter().map(|f| f.label.as_str()).collect();
	for f in &findings {
		eprintln!(
			"{:<58} seed {:>5}  hits {:>5}  repro {} calls",
			f.label,
			f.seed,
			f.hits,
			f.reproduction.len()
		);
	}

	let expected = [
		// ESC-01: the arbiter's refund lands in the arbiter's account.
		"refund: balances moved differently from spec",
		// ESC-02: a cancelled escrow stays live, so it can be cancelled or released again.
		"cancel: succeeded but no live escrow",
		// ESC-03: milestone total overflows (a wrap in release builds).
		"create: arithmetic overflow panic",
		// ESC-04: anyone can release.
		"release: succeeded but caller may not release",
		// ESC-05: the payer can take the money back early.
		"refund: succeeded but payer refunded before the deadline",
		// ESC-06: panics on a missing id and after the last milestone.
		"release: unwrap on missing escrow panic",
		"release: index out of bounds panic",
	];
	let missing: Vec<_> = expected.iter().filter(|e| !labels.contains(e)).collect();
	assert!(missing.is_empty(), "harness missed: {missing:?}\nfound: {labels:#?}");
}
