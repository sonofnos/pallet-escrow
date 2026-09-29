//! The written escrow spec as an executable oracle.
//!
//! The model does not re-implement the pallet. It only answers two questions for each call:
//! is this call allowed by the spec, and if it succeeds, exactly which balances move and by how
//! much. The driver then holds the pallet to three rules:
//!
//! 1. A call that returns `Ok` must be allowed by the spec and have exactly the spec's effect on
//!    every account's free balance and both hold balances.
//! 2. A call that returns `Err` must change nothing.
//! 3. No call may panic, and total issuance never changes.

use arbitrary::Arbitrary;
use sp_runtime::DispatchResult;
use std::{
	collections::BTreeMap,
	fmt,
	panic::{catch_unwind, AssertUnwindSafe},
};

use crate::runtimes::ACCOUNTS;

/// Limits the spec is checked against. They mirror each target's pallet configuration.
#[derive(Clone, Copy, Debug)]
pub struct SpecParams {
	pub min_milestone: u64,
	pub deposit: u64,
	pub max_milestones: usize,
}

/// A pallet under test, wrapped in its own mock runtime.
pub trait Target {
	const NAME: &'static str;
	const PARAMS: SpecParams;

	fn new_ext() -> sp_io::TestExternalities;
	fn create(
		payer: u64,
		beneficiary: u64,
		arbiter: Option<u64>,
		milestones: Vec<u64>,
		deadline: u64,
	) -> DispatchResult;
	fn release(who: u64, id: u32) -> DispatchResult;
	fn refund(who: u64, id: u32) -> DispatchResult;
	fn cancel(who: u64, id: u32) -> DispatchResult;
	fn block() -> u64;
	fn set_block(n: u64);
	fn free(who: u64) -> u64;
	fn held_escrow(who: u64) -> u64;
	fn held_deposit(who: u64) -> u64;
	fn total_issuance() -> u64;
	fn next_id() -> u32;
	/// The pallet's own invariant check, if it has one.
	fn try_state() -> Result<(), String> {
		Ok(())
	}
}

/// Milestone amounts: mostly realistic, sometimes close to `u64::MAX` to reach overflow.
#[derive(Arbitrary, Clone, Copy, Debug)]
pub enum Amount {
	Small(u16),
	Huge(u16),
}

impl Amount {
	fn value(self) -> u64 {
		match self {
			Amount::Small(v) => u64::from(v % 3_000),
			Amount::Huge(v) => u64::MAX - u64::from(v),
		}
	}
}

/// One call, with accounts and escrow ids as small indices that the driver maps onto live state.
#[derive(Arbitrary, Clone, Debug)]
pub enum Action {
	Create {
		payer: u8,
		beneficiary: u8,
		arbiter: Option<u8>,
		milestones: Vec<Amount>,
		deadline_in: u8,
	},
	Release {
		who: u8,
		id: u8,
	},
	Refund {
		who: u8,
		id: u8,
	},
	Cancel {
		who: u8,
		id: u8,
	},
	Advance {
		blocks: u8,
	},
}

fn account(index: u8) -> u64 {
	u64::from(index) % ACCOUNTS + 1
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
	/// The pallet panicked. Message included.
	Panic(String),
	/// The call succeeded although the spec forbids it.
	SpecForbidden(&'static str),
	/// The call succeeded but moved different amounts than the spec says.
	BalanceMismatch,
	/// The call failed but still changed state.
	StateChangedOnError,
	/// Total issuance changed.
	IssuanceChanged,
	/// Escrow ids were not assigned sequentially.
	IdMismatch,
	/// The pallet's own `try_state` check failed.
	TryState(String),
}

#[derive(Clone, Debug)]
pub struct Violation {
	pub step: usize,
	pub action: Action,
	pub kind: Kind,
	pub detail: String,
}

impl Violation {
	/// A short, stable name for the class of bug, used to group findings.
	pub fn label(&self) -> String {
		let call = match self.action {
			Action::Create { .. } => "create",
			Action::Release { .. } => "release",
			Action::Refund { .. } => "refund",
			Action::Cancel { .. } => "cancel",
			Action::Advance { .. } => "advance",
		};
		match &self.kind {
			Kind::Panic(msg) if msg.contains("overflow") => {
				format!("{call}: arithmetic overflow panic")
			},
			Kind::Panic(msg) if msg.contains("index out of bounds") => {
				format!("{call}: index out of bounds panic")
			},
			Kind::Panic(msg) if msg.contains("unwrap") || msg.contains("None") => {
				format!("{call}: unwrap on missing escrow panic")
			},
			Kind::Panic(_) => format!("{call}: panic"),
			Kind::SpecForbidden(why) => format!("{call}: succeeded but {why}"),
			Kind::BalanceMismatch => format!("{call}: balances moved differently from spec"),
			Kind::StateChangedOnError => format!("{call}: failed but changed state"),
			Kind::IssuanceChanged => format!("{call}: total issuance changed"),
			Kind::IdMismatch => format!("{call}: escrow id not sequential"),
			Kind::TryState(_) => format!("{call}: try_state failed"),
		}
	}
}

impl fmt::Display for Violation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "step {}: {} ({:?})\n  {}", self.step, self.label(), self.action, self.detail)
	}
}

#[derive(Clone, Debug)]
struct ModelEscrow {
	payer: u64,
	beneficiary: u64,
	arbiter: Option<u64>,
	milestones: Vec<u64>,
	released: usize,
	remaining: u64,
	deposit: u64,
	deadline: u64,
}

/// Per-account `(free, escrow hold, deposit hold)`.
type Balances = BTreeMap<u64, (u64, u64, u64)>;

fn snapshot<T: Target>() -> Balances {
	(1..=ACCOUNTS)
		.map(|who| (who, (T::free(who), T::held_escrow(who), T::held_deposit(who))))
		.collect()
}

/// Signed balance changes the spec expects from one successful call.
#[derive(Default)]
struct Effect(BTreeMap<u64, (i128, i128, i128)>);

impl Effect {
	fn add(&mut self, who: u64, free: i128, held: i128, deposit: i128) {
		let e = self.0.entry(who).or_default();
		e.0 += free;
		e.1 += held;
		e.2 += deposit;
	}

	fn apply(&self, before: &Balances) -> Option<Balances> {
		let mut out = before.clone();
		for (who, (free, held, deposit)) in &self.0 {
			let row = out.get_mut(who)?;
			row.0 = u64::try_from(i128::from(row.0) + free).ok()?;
			row.1 = u64::try_from(i128::from(row.1) + held).ok()?;
			row.2 = u64::try_from(i128::from(row.2) + deposit).ok()?;
		}
		Some(out)
	}
}

#[derive(Default)]
struct Model {
	escrows: BTreeMap<u32, ModelEscrow>,
	next_id: u32,
}

impl Model {
	/// What the spec says a *successful* call must do, or why it must not succeed.
	fn expect(&self, call: &Call, block: u64, p: SpecParams) -> Result<Effect, &'static str> {
		let mut fx = Effect::default();
		match call {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
				if payer == beneficiary {
					return Err("payer is the beneficiary");
				}
				if arbiter.is_some_and(|a| a == *payer || a == *beneficiary) {
					return Err("arbiter is a party");
				}
				if milestones.is_empty() {
					return Err("no milestones");
				}
				if milestones.len() > p.max_milestones {
					return Err("more milestones than the bound");
				}
				if milestones.iter().any(|m| *m < p.min_milestone) {
					return Err("milestone below minimum");
				}
				if *deadline <= block {
					return Err("deadline not in the future");
				}
				let total = milestones
					.iter()
					.try_fold(0u64, |acc, m| acc.checked_add(*m))
					.ok_or("milestone total overflows")?;
				let (total, deposit) = (i128::from(total), i128::from(p.deposit));
				fx.add(*payer, -(total + deposit), total, deposit);
			},
			Call::Release { who, id } => {
				let e = self.escrows.get(id).ok_or("no live escrow")?;
				if *who != e.payer && Some(*who) != e.arbiter {
					return Err("caller may not release");
				}
				let amount = i128::from(e.milestones[e.released]);
				fx.add(e.payer, 0, -amount, 0);
				fx.add(e.beneficiary, amount, 0, 0);
				if e.released + 1 == e.milestones.len() {
					let deposit = i128::from(e.deposit);
					fx.add(e.payer, deposit, 0, -deposit);
				}
			},
			Call::Refund { who, id } => {
				let e = self.escrows.get(id).ok_or("no live escrow")?;
				if Some(*who) != e.arbiter {
					if *who != e.payer {
						return Err("caller may not refund");
					}
					if block < e.deadline {
						return Err("payer refunded before the deadline");
					}
				}
				Self::close(&mut fx, e);
			},
			Call::Cancel { who, id } => {
				let e = self.escrows.get(id).ok_or("no live escrow")?;
				if *who != e.beneficiary {
					return Err("caller may not cancel");
				}
				Self::close(&mut fx, e);
			},
		}
		Ok(fx)
	}

	fn close(fx: &mut Effect, e: &ModelEscrow) {
		let (remaining, deposit) = (i128::from(e.remaining), i128::from(e.deposit));
		fx.add(e.payer, remaining + deposit, -remaining, -deposit);
	}

	/// Record a successful call. Only called once the effect has been checked.
	fn commit(&mut self, call: &Call, p: SpecParams) {
		match call {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
				self.escrows.insert(
					self.next_id,
					ModelEscrow {
						payer: *payer,
						beneficiary: *beneficiary,
						arbiter: *arbiter,
						milestones: milestones.clone(),
						released: 0,
						remaining: milestones.iter().sum(),
						deposit: p.deposit,
						deadline: *deadline,
					},
				);
				self.next_id += 1;
			},
			Call::Release { id, .. } => {
				let e = self.escrows.get_mut(id).expect("checked in expect");
				e.remaining -= e.milestones[e.released];
				e.released += 1;
				if e.released == e.milestones.len() {
					self.escrows.remove(id);
				}
			},
			Call::Refund { id, .. } | Call::Cancel { id, .. } => {
				self.escrows.remove(id);
			},
		}
	}
}

/// An [`Action`] resolved against live state.
#[derive(Debug)]
enum Call {
	Create {
		payer: u64,
		beneficiary: u64,
		arbiter: Option<u64>,
		milestones: Vec<u64>,
		deadline: u64,
	},
	Release {
		who: u64,
		id: u32,
	},
	Refund {
		who: u64,
		id: u32,
	},
	Cancel {
		who: u64,
		id: u32,
	},
}

/// Longest sequence the driver will run; fuzz inputs are truncated to this.
pub const MAX_ACTIONS: usize = 48;
const MAX_MILESTONES_GENERATED: usize = 12;

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
	if let Some(s) = payload.downcast_ref::<&str>() {
		(*s).to_string()
	} else if let Some(s) = payload.downcast_ref::<String>() {
		s.clone()
	} else {
		"non-string panic payload".to_string()
	}
}

/// Run one call sequence against a fresh chain and return the first spec violation, if any.
pub fn run<T: Target>(actions: &[Action]) -> Result<(), Violation> {
	let mut ext = T::new_ext();
	ext.execute_with(|| {
		let issuance = T::total_issuance();
		let mut model = Model::default();

		for (step, action) in actions.iter().take(MAX_ACTIONS).enumerate() {
			let fail =
				|kind, detail: String| Violation { step, action: action.clone(), kind, detail };
			let block = T::block();
			let span = model.next_id + 1;
			let call = match action.clone() {
				Action::Advance { blocks } => {
					T::set_block(block + u64::from(blocks));
					continue;
				},
				Action::Create { payer, beneficiary, arbiter, milestones, deadline_in } => {
					Call::Create {
						payer: account(payer),
						beneficiary: account(beneficiary),
						arbiter: arbiter.map(account),
						milestones: milestones
							.iter()
							.take(MAX_MILESTONES_GENERATED)
							.map(|m| m.value())
							.collect(),
						// Spans a little into the past so past deadlines are exercised too.
						deadline: (block + u64::from(deadline_in % 24)).saturating_sub(2),
					}
				},
				Action::Release { who, id } => {
					Call::Release { who: account(who), id: u32::from(id) % span }
				},
				Action::Refund { who, id } => {
					Call::Refund { who: account(who), id: u32::from(id) % span }
				},
				Action::Cancel { who, id } => {
					Call::Cancel { who: account(who), id: u32::from(id) % span }
				},
			};

			let before = snapshot::<T>();
			let outcome = catch_unwind(AssertUnwindSafe(|| match &call {
				Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
					T::create(*payer, *beneficiary, *arbiter, milestones.clone(), *deadline)
				},
				Call::Release { who, id } => T::release(*who, *id),
				Call::Refund { who, id } => T::refund(*who, *id),
				Call::Cancel { who, id } => T::cancel(*who, *id),
			}));
			let outcome = match outcome {
				Ok(outcome) => outcome,
				Err(payload) => {
					let msg = panic_message(payload);
					return Err(fail(
						Kind::Panic(msg.clone()),
						format!("{call:?} panicked: {msg}"),
					));
				},
			};
			let after = snapshot::<T>();

			match outcome {
				Err(err) => {
					if after != before {
						return Err(fail(
							Kind::StateChangedOnError,
							format!("{call:?} returned {err:?}\n  before {before:?}\n  after  {after:?}"),
						));
					}
				},
				Ok(()) => {
					let effect = model.expect(&call, block, T::PARAMS).map_err(|why| {
						fail(Kind::SpecForbidden(why), format!("{call:?} returned Ok"))
					})?;
					let expected = effect.apply(&before);
					if expected.as_ref() != Some(&after) {
						return Err(fail(
							Kind::BalanceMismatch,
							format!("{call:?}\n  expected {expected:?}\n  actual   {after:?}"),
						));
					}
					model.commit(&call, T::PARAMS);
					if T::next_id() != model.next_id {
						return Err(fail(
							Kind::IdMismatch,
							format!("pallet next id {} model {}", T::next_id(), model.next_id),
						));
					}
				},
			}

			if T::total_issuance() != issuance {
				return Err(fail(
					Kind::IssuanceChanged,
					format!("{issuance} -> {}", T::total_issuance()),
				));
			}
			if let Err(e) = T::try_state() {
				return Err(fail(Kind::TryState(e.clone()), e));
			}
		}
		Ok(())
	})
}

/// Drop actions one at a time while the sequence still fails with the same label.
pub fn shrink<T: Target>(actions: &[Action], violation: &Violation) -> (Vec<Action>, Violation) {
	let label = violation.label();
	let mut best = actions[..=violation.step.min(actions.len() - 1)].to_vec();
	let mut best_violation = violation.clone();
	let mut i = 0;
	while i < best.len() {
		let mut candidate = best.clone();
		candidate.remove(i);
		match run::<T>(&candidate) {
			Err(v) if v.label() == label => {
				best = candidate;
				best_violation = v;
			},
			_ => i += 1,
		}
	}
	(best, best_violation)
}
