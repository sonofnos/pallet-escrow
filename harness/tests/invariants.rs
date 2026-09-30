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
	escrow_harness::spec::reset_coverage();
	let findings = hunt::<Fixed>(seeds(4_000));
	eprintln!("pallet-escrow calls:\n{}", escrow_harness::spec::coverage());
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
	escrow_harness::spec::reset_coverage();
	let findings = hunt::<V0>(seeds(4_000));
	eprintln!("pallet-escrow-v0 calls:\n{}", escrow_harness::spec::coverage());
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

	// Each audit finding, and the labels under which the harness can surface it. A sequence
	// stops at its first violation, so a finding can show up through any of its consequences.
	let expected: [(&str, &[&str]); 7] = [
		(
			"ESC-01 arbiter refund pays the arbiter",
			&["refund: balances moved differently from spec"],
		),
		(
			"ESC-02 cancelled escrow stays live",
			&[
				"cancel: closed escrow left in storage",
				"cancel: succeeded but no live escrow",
				"release: succeeded but no live escrow",
				"refund: succeeded but no live escrow",
			],
		),
		("ESC-03 milestone total overflows", &["create: arithmetic overflow panic"]),
		("ESC-04 anyone can release", &["release: succeeded but caller may not release"]),
		(
			"ESC-05 payer refunds before the deadline",
			&["refund: succeeded but payer refunded before the deadline"],
		),
		("ESC-06a panic on unknown id", &["release: unwrap on missing escrow panic"]),
		(
			"ESC-06b panic after the last milestone",
			&["release: closed escrow left in storage", "release: index out of bounds panic"],
		),
	];
	let missing: Vec<&str> = expected
		.iter()
		.filter(|(_, alternatives)| !alternatives.iter().any(|a| labels.contains(a)))
		.map(|(finding, _)| *finding)
		.collect();
	assert!(missing.is_empty(), "harness missed: {missing:?}\nfound: {labels:#?}");
}
