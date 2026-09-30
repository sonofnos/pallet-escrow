//! The written escrow spec as an executable oracle.
//!
//! The model does not re-implement the pallet. For each call it answers two questions: must this
//! call fail, and for which reasons; and if it must not, exactly what does it do to balances,
//! escrow storage and events. The driver then holds the pallet to four rules:
//!
//! 1. A call the spec gives no reason to reject must return `Ok` and have exactly the spec's
//!    effect on every account's free balance and both hold balances, on the stored escrows and
//!    on the events emitted.
//! 2. A call the spec rejects must return `Err` with an error that is one of the spec's reasons,
//!    and change nothing.
//! 3. No call may panic, and total issuance never changes.
//! 4. The pallet's own `try_state` holds after every call.
//!
//! Rule 1 is what makes the oracle two-sided: a pallet that refuses a call it owes the caller
//! (funds that can never be paid out or refunded) fails it as surely as one that pays the wrong
//! account.

use arbitrary::Arbitrary;
use sp_runtime::{DispatchError, DispatchResult, ModuleError};
use std::{
	collections::BTreeMap,
	fmt,
	panic::{catch_unwind, AssertUnwindSafe},
	sync::atomic::{AtomicU64, Ordering},
};

/// Calls executed, as `[ok, err]` for create, release, refund, cancel. Reported next to the
/// results so a clean run can be told apart from a run that never reached deep states.
static CALLS: [[AtomicU64; 2]; 4] = [
	[AtomicU64::new(0), AtomicU64::new(0)],
	[AtomicU64::new(0), AtomicU64::new(0)],
	[AtomicU64::new(0), AtomicU64::new(0)],
	[AtomicU64::new(0), AtomicU64::new(0)],
];

/// Reset the call counters.
pub fn reset_coverage() {
	CALLS.iter().flatten().for_each(|c| c.store(0, Ordering::Relaxed));
}

/// One line per call kind: how many succeeded and how many were rejected.
pub fn coverage() -> String {
	["create", "release", "refund", "cancel"]
		.iter()
		.zip(CALLS.iter())
		.map(|(name, [ok, err])| {
			format!(
				"{name:<8} ok {:>8}  rejected {:>8}",
				ok.load(Ordering::Relaxed),
				err.load(Ordering::Relaxed)
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

use crate::runtimes::{ACCOUNTS, START_BALANCE};

/// Limits the spec is checked against. They mirror each target's pallet configuration.
#[derive(Clone, Copy, Debug)]
pub struct SpecParams {
	pub min_milestone: u64,
	pub deposit: u64,
	pub max_milestones: usize,
	pub existential_deposit: u64,
}

/// Returned by a target for a `create` whose milestones exceed the bound: such a call never
/// decodes, so it cannot reach the pallet.
pub const UNDECODABLE: &str = "call does not decode: too many milestones";

/// An escrow as the spec sees it. Targets report their storage in this shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
	pub payer: u64,
	pub beneficiary: u64,
	pub arbiter: Option<u64>,
	pub milestones: Vec<u64>,
	pub released: u32,
	pub remaining: u64,
	pub deposit: u64,
	pub deadline: u64,
}

/// The pallet's events, as the spec sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecEvent {
	Created { id: u32, payer: u64, beneficiary: u64, total: u64 },
	MilestoneReleased { id: u32, index: u32, amount: u64 },
	Completed { id: u32 },
	Refunded { id: u32, amount: u64 },
	Cancelled { id: u32, amount: u64 },
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
	fn escrows() -> BTreeMap<u32, Record>;
	/// Escrow events since the last [`Target::reset_events`], or `None` if this target's events
	/// are not held to the spec.
	fn events() -> Option<Vec<SpecEvent>>;
	fn reset_events();
	/// Set or replace the one lock another pallet keeps on `who`.
	fn lock(who: u64, amount: u64);
	fn unlock(who: u64);
	/// A plain transfer outside the escrow pallet. It may fail; the spec does not care.
	fn transfer(from: u64, to: u64, amount: u64);
	/// The pallet's own invariant check, if it has one.
	fn try_state() -> Result<(), String> {
		Ok(())
	}
}

/// Why the spec rejects a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
	NotFound,
	NotAuthorized,
	SelfEscrow,
	ArbiterConflict,
	NoMilestones,
	TooManyMilestones,
	MilestoneTooSmall,
	DeadlineInPast,
	DeadlineNotReached,
	Overflow,
	/// The payer cannot put the escrow on hold without going below what the ledger requires
	/// them to keep.
	Funds,
}

impl Reason {
	fn describe(self, call: &str) -> String {
		match self {
			Reason::NotFound => "no live escrow".into(),
			Reason::NotAuthorized => format!("caller may not {call}"),
			Reason::SelfEscrow => "payer is the beneficiary".into(),
			Reason::ArbiterConflict => "arbiter is a party".into(),
			Reason::NoMilestones => "no milestones".into(),
			Reason::TooManyMilestones => "more milestones than the bound".into(),
			Reason::MilestoneTooSmall => "milestone below minimum".into(),
			Reason::DeadlineInPast => "deadline not in the future".into(),
			Reason::DeadlineNotReached => "payer refunded before the deadline".into(),
			Reason::Overflow => "milestone total overflows".into(),
			Reason::Funds => "payer cannot afford it".into(),
		}
	}

	/// The reason a dispatch error stands for. Both pallets name their errors the same way.
	fn of(err: DispatchError) -> Option<Reason> {
		match err {
			DispatchError::Module(ModuleError { message: Some(name), .. }) => match name {
				"NotFound" => Some(Reason::NotFound),
				"NotAuthorized" => Some(Reason::NotAuthorized),
				"SelfEscrow" => Some(Reason::SelfEscrow),
				"ArbiterConflict" => Some(Reason::ArbiterConflict),
				"NoMilestones" => Some(Reason::NoMilestones),
				"MilestoneTooSmall" => Some(Reason::MilestoneTooSmall),
				"DeadlineInPast" => Some(Reason::DeadlineInPast),
				"DeadlineNotReached" => Some(Reason::DeadlineNotReached),
				"Overflow" => Some(Reason::Overflow),
				_ => None,
			},
			DispatchError::Token(_) => Some(Reason::Funds),
			DispatchError::Other(UNDECODABLE) => Some(Reason::TooManyMilestones),
			_ => None,
		}
	}
}

/// A milestone amount: usually realistic, about 3% of the time close to `u64::MAX` so that
/// overflow is reachable without drowning every sequence in it.
#[derive(Arbitrary, Clone, Copy, Debug)]
pub struct Amount {
	pub value: u16,
	pub huge: u8,
}

impl Amount {
	fn value(self, well_formed: bool, min: u64) -> u64 {
		if self.huge < 8 {
			u64::MAX - u64::from(self.value)
		} else if well_formed {
			min + u64::from(self.value) % 1_500
		} else {
			u64::from(self.value % 3_000)
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
		/// Three in four creates are well formed, so sequences reach deep states instead of
		/// stopping at input validation; the rest take the raw values.
		shape: u8,
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
	/// Another pallet locks part of an account's balance, as conviction voting does. A lock
	/// applies to the whole balance, held funds included.
	Lock {
		who: u8,
		amount: u16,
	},
	Unlock {
		who: u8,
	},
	/// An account pays someone outside any escrow: an exact amount, everything down to the
	/// existential deposit, or everything.
	Spend {
		who: u8,
		to: u8,
		amount: u16,
		mode: u8,
	},
}

fn account(index: u8) -> u64 {
	u64::from(index) % ACCOUNTS + 1
}

/// An account other than `base`, chosen by `n`.
fn other_account(base: u64, n: u8) -> u64 {
	(base + u64::from(n) % (ACCOUNTS - 1)) % ACCOUNTS + 1
}

/// Pick a caller by role in the escrow, so authorised and unauthorised calls both happen often.
fn caller(who: u8, parties: Option<&(u64, u64, Option<u64>)>) -> u64 {
	match (parties, who % 6) {
		(Some((payer, _, _)), 0 | 1) => *payer,
		(Some((_, beneficiary, _)), 2) => *beneficiary,
		(Some((payer, beneficiary, arbiter)), 3) => arbiter.unwrap_or_else(|| {
			(1..=ACCOUNTS).find(|a| a != payer && a != beneficiary).unwrap_or(1)
		}),
		_ => account(who / 6),
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
	/// The pallet panicked. Message included.
	Panic(String),
	/// The call succeeded although the spec rejects it, for this reason first.
	SpecForbidden(Reason),
	/// The call failed although the spec gives no reason to reject it.
	Rejected,
	/// The call failed, but not for any reason the spec gives.
	WrongReason,
	/// The call succeeded but moved different amounts than the spec says.
	BalanceMismatch,
	/// The call succeeded but emitted different events than the spec says.
	EventMismatch,
	/// The call succeeded but left an escrow the spec says is closed.
	ClosedEscrowLeft,
	/// The call succeeded but the stored escrows differ from the spec's.
	StorageMismatch,
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
			Action::Lock { .. } => "lock",
			Action::Unlock { .. } => "unlock",
			Action::Spend { .. } => "spend",
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
			Kind::SpecForbidden(why) => format!("{call}: succeeded but {}", why.describe(call)),
			Kind::Rejected => format!("{call}: rejected although the spec requires success"),
			Kind::WrongReason => format!("{call}: rejected for a reason the spec does not give"),
			Kind::BalanceMismatch => format!("{call}: balances moved differently from spec"),
			Kind::EventMismatch => format!("{call}: events differ from spec"),
			Kind::ClosedEscrowLeft => format!("{call}: closed escrow left in storage"),
			Kind::StorageMismatch => format!("{call}: escrow storage differs from spec"),
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

/// Per-account `(free, escrow hold, deposit hold)`.
type Balances = BTreeMap<u64, (u64, u64, u64)>;

/// Everything a call can change that the spec has an opinion on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
	balances: Balances,
	escrows: BTreeMap<u32, Record>,
	next_id: u32,
	events: Option<Vec<SpecEvent>>,
}

fn snapshot<T: Target>() -> State {
	State {
		balances: (1..=ACCOUNTS)
			.map(|who| (who, (T::free(who), T::held_escrow(who), T::held_deposit(who))))
			.collect(),
		escrows: T::escrows(),
		next_id: T::next_id(),
		events: T::events(),
	}
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

/// What a successful call must do.
#[derive(Default)]
struct Expected {
	effect: Effect,
	events: Vec<SpecEvent>,
}

#[derive(Default)]
struct Model {
	escrows: BTreeMap<u32, Record>,
	next_id: u32,
	/// Parties of every escrow ever created, live or closed.
	parties: BTreeMap<u32, (u64, u64, Option<u64>)>,
}

/// Whether `who` can move `amount` from free balance onto hold.
///
/// The ledger lets an account hold funds as long as its free balance stays at or above the
/// existential deposit.
fn can_hold(balances: &Balances, who: u64, amount: u64, p: SpecParams) -> bool {
	let (free, ..) = balances[&who];
	amount.checked_add(p.existential_deposit).is_some_and(|need| free >= need)
}

impl Model {
	/// What a call must do, or every reason the spec gives for rejecting it.
	fn expect(
		&self,
		call: &Call,
		block: u64,
		p: SpecParams,
		before: &Balances,
	) -> Result<Expected, Vec<Reason>> {
		let mut out = Expected::default();
		match call {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
				let mut why = Vec::new();
				if payer == beneficiary {
					why.push(Reason::SelfEscrow);
				}
				if arbiter.is_some_and(|a| a == *payer || a == *beneficiary) {
					why.push(Reason::ArbiterConflict);
				}
				if milestones.is_empty() {
					why.push(Reason::NoMilestones);
				}
				if milestones.len() > p.max_milestones {
					why.push(Reason::TooManyMilestones);
				}
				if milestones.iter().any(|m| *m < p.min_milestone) {
					why.push(Reason::MilestoneTooSmall);
				}
				if *deadline <= block {
					why.push(Reason::DeadlineInPast);
				}
				let total = milestones.iter().try_fold(0u64, |acc, m| acc.checked_add(*m));
				match total {
					None => why.push(Reason::Overflow),
					Some(total) => {
						let affordable = total
							.checked_add(p.deposit)
							.is_some_and(|need| can_hold(before, *payer, need, p));
						if !affordable {
							why.push(Reason::Funds);
						}
					},
				}
				if !why.is_empty() {
					return Err(why);
				}
				let total = total.expect("checked above");
				let (t, d) = (i128::from(total), i128::from(p.deposit));
				out.effect.add(*payer, -(t + d), t, d);
				out.events.push(SpecEvent::Created {
					id: self.next_id,
					payer: *payer,
					beneficiary: *beneficiary,
					total,
				});
			},
			Call::Release { who, id } => {
				let e = self.escrows.get(id).ok_or(vec![Reason::NotFound])?;
				if *who != e.payer && Some(*who) != e.arbiter {
					return Err(vec![Reason::NotAuthorized]);
				}
				let index = e.released;
				let amount = e.milestones[index as usize];
				out.effect.add(e.payer, 0, -i128::from(amount), 0);
				out.effect.add(e.beneficiary, i128::from(amount), 0, 0);
				out.events.push(SpecEvent::MilestoneReleased { id: *id, index, amount });
				if index as usize + 1 == e.milestones.len() {
					let deposit = i128::from(e.deposit);
					out.effect.add(e.payer, deposit, 0, -deposit);
					out.events.push(SpecEvent::Completed { id: *id });
				}
			},
			Call::Refund { who, id } => {
				let e = self.escrows.get(id).ok_or(vec![Reason::NotFound])?;
				if Some(*who) != e.arbiter {
					if *who != e.payer {
						return Err(vec![Reason::NotAuthorized]);
					}
					if block < e.deadline {
						return Err(vec![Reason::DeadlineNotReached]);
					}
				}
				Self::close(&mut out.effect, e);
				out.events.push(SpecEvent::Refunded { id: *id, amount: e.remaining });
			},
			Call::Cancel { who, id } => {
				let e = self.escrows.get(id).ok_or(vec![Reason::NotFound])?;
				if *who != e.beneficiary {
					return Err(vec![Reason::NotAuthorized]);
				}
				Self::close(&mut out.effect, e);
				out.events.push(SpecEvent::Cancelled { id: *id, amount: e.remaining });
			},
		}
		Ok(out)
	}

	fn close(fx: &mut Effect, e: &Record) {
		let (remaining, deposit) = (i128::from(e.remaining), i128::from(e.deposit));
		fx.add(e.payer, remaining + deposit, -remaining, -deposit);
	}

	/// Record a successful call. Only called once the effect has been checked.
	fn commit(&mut self, call: &Call, p: SpecParams) {
		match call {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
				self.escrows.insert(
					self.next_id,
					Record {
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
				self.parties.insert(self.next_id, (*payer, *beneficiary, *arbiter));
				self.next_id += 1;
			},
			Call::Release { id, .. } => {
				let e = self.escrows.get_mut(id).expect("checked in expect");
				e.remaining -= e.milestones[e.released as usize];
				e.released += 1;
				if e.released as usize == e.milestones.len() {
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

thread_local! {
	/// Set while a pallet call runs, so [`crate::quiet_panics`] silences only its panics.
	pub(crate) static IN_CALL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
				Action::Lock { who, amount } => {
					T::lock(account(who), u64::from(amount) % (2 * START_BALANCE));
					continue;
				},
				Action::Unlock { who } => {
					T::unlock(account(who));
					continue;
				},
				Action::Spend { who, to, amount, mode } => {
					let (from, free) = (account(who), T::free(account(who)));
					let amount = match mode % 3 {
						0 => u64::from(amount) % (free + 1),
						1 => free.saturating_sub(T::PARAMS.existential_deposit),
						_ => free,
					};
					T::transfer(from, other_account(from, to), amount);
					continue;
				},
				Action::Create { payer, beneficiary, arbiter, milestones, deadline_in, shape } => {
					let well_formed = shape % 4 != 0;
					let min = T::PARAMS.min_milestone;
					let payer = account(payer);
					let mut amounts: Vec<u64> = milestones
						.iter()
						.take(MAX_MILESTONES_GENERATED)
						.map(|m| m.value(well_formed, min))
						.collect();
					if well_formed {
						let beneficiary = other_account(payer, beneficiary);
						amounts.truncate(T::PARAMS.max_milestones);
						if amounts.is_empty() {
							amounts.push(min);
						}
						let third_parties: Vec<u64> =
							(1..=ACCOUNTS).filter(|x| *x != payer && *x != beneficiary).collect();
						Call::Create {
							payer,
							beneficiary,
							arbiter: arbiter
								.map(|a| third_parties[usize::from(a) % third_parties.len()]),
							milestones: amounts,
							deadline: block + 1 + u64::from(deadline_in % 20),
						}
					} else {
						Call::Create {
							payer,
							beneficiary: account(beneficiary),
							arbiter: arbiter.map(account),
							milestones: amounts,
							// Spans a little into the past so past deadlines are exercised too.
							deadline: (block + u64::from(deadline_in % 24)).saturating_sub(2),
						}
					}
				},
				Action::Release { who, id } => {
					let id = u32::from(id) % span;
					Call::Release { who: caller(who, model.parties.get(&id)), id }
				},
				Action::Refund { who, id } => {
					let id = u32::from(id) % span;
					Call::Refund { who: caller(who, model.parties.get(&id)), id }
				},
				Action::Cancel { who, id } => {
					let id = u32::from(id) % span;
					Call::Cancel { who: caller(who, model.parties.get(&id)), id }
				},
			};

			T::reset_events();
			let before = snapshot::<T>();
			IN_CALL.with(|c| c.set(true));
			let outcome = catch_unwind(AssertUnwindSafe(|| match &call {
				Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
					T::create(*payer, *beneficiary, *arbiter, milestones.clone(), *deadline)
				},
				Call::Release { who, id } => T::release(*who, *id),
				Call::Refund { who, id } => T::refund(*who, *id),
				Call::Cancel { who, id } => T::cancel(*who, *id),
			}));
			IN_CALL.with(|c| c.set(false));
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
			let kind = match call {
				Call::Create { .. } => 0,
				Call::Release { .. } => 1,
				Call::Refund { .. } => 2,
				Call::Cancel { .. } => 3,
			};
			CALLS[kind][usize::from(outcome.is_err())].fetch_add(1, Ordering::Relaxed);

			let expected = model.expect(&call, block, T::PARAMS, &before.balances);
			match (outcome, expected) {
				(Err(err), Ok(_)) => {
					return Err(fail(
						Kind::Rejected,
						format!("{call:?} returned {err:?}, the spec gives no reason to reject it"),
					));
				},
				(Err(err), Err(reasons)) => {
					if !Reason::of(err).is_some_and(|r| reasons.contains(&r)) {
						return Err(fail(
							Kind::WrongReason,
							format!(
								"{call:?} returned {err:?}, the spec's reasons are {reasons:?}"
							),
						));
					}
					if after != before {
						return Err(fail(
							Kind::StateChangedOnError,
							format!("{call:?} returned {err:?}\n  before {before:?}\n  after  {after:?}"),
						));
					}
				},
				(Ok(()), Err(reasons)) => {
					return Err(fail(
						Kind::SpecForbidden(reasons[0]),
						format!("{call:?} returned Ok, the spec's reasons are {reasons:?}"),
					));
				},
				(Ok(()), Ok(expected)) => {
					let balances = expected.effect.apply(&before.balances);
					if balances.as_ref() != Some(&after.balances) {
						return Err(fail(
							Kind::BalanceMismatch,
							format!(
								"{call:?}\n  expected {balances:?}\n  actual   {:?}",
								after.balances
							),
						));
					}
					if after.events.as_ref().is_some_and(|events| *events != expected.events) {
						return Err(fail(
							Kind::EventMismatch,
							format!(
								"{call:?}\n  expected {:?}\n  actual   {:?}",
								expected.events, after.events
							),
						));
					}
					model.commit(&call, T::PARAMS);
					if after.next_id != model.next_id {
						return Err(fail(
							Kind::IdMismatch,
							format!("pallet next id {} model {}", after.next_id, model.next_id),
						));
					}
					if after.escrows != model.escrows {
						let left = after.escrows.keys().find(|id| !model.escrows.contains_key(id));
						let kind = if left.is_some() {
							Kind::ClosedEscrowLeft
						} else {
							Kind::StorageMismatch
						};
						return Err(fail(
							kind,
							format!(
								"{call:?}\n  expected {:?}\n  actual   {:?}",
								model.escrows, after.escrows
							),
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
