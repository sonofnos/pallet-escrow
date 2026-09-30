//! The written escrow spec as an executable oracle.
//!
//! The model does not re-implement the pallet. For each call it answers two questions: must this
//! call fail, and for which reasons; and if it must not, exactly what does it do to balances,
//! escrow storage and events. The driver then holds the pallet to four rules:
//!
//! 1. A call the spec gives no reason to reject must return `Ok` and have exactly the spec's
//!    effect on every account's free balance, the funds escrowed on its behalf and its storage
//!    deposits, on the stored escrows and on the events emitted.
//! 2. A call the spec rejects must return `Err` with an error that is one of the spec's reasons,
//!    and change nothing.
//! 3. No call may panic, and total issuance never changes.
//! 4. The pallet's own `try_state` holds after every call.
//!
//! Rule 1 is what makes the oracle two-sided: a pallet that refuses a call it owes the caller
//! (funds that can never be paid out or refunded) fails it as surely as one that pays the wrong
//! account.
//!
//! One spec covers every target. The classic targets (`v0`, `v1`) have no claims and a single
//! deadline per escrow; the driver never sends them claim calls and gives all of an escrow's
//! milestones the same deadline, and on that subset the claim rules never apply.

use arbitrary::Arbitrary;
use sp_runtime::{ArithmeticError, DispatchError, DispatchResult, ModuleError};
use std::{
	collections::BTreeMap,
	fmt,
	panic::{catch_unwind, AssertUnwindSafe},
	sync::atomic::{AtomicU64, Ordering},
};

use crate::runtimes::{ACCOUNTS, START_BALANCE};

/// Amounts in the spec. Targets with narrower balances say so in [`SpecParams::max_balance`].
pub type Balance = u128;

const CALL_NAMES: [&str; 8] =
	["create", "release", "refund", "cancel", "submit", "dispute", "claim", "resolve"];

/// Calls executed, as `[ok, err]` per entry of `CALL_NAMES`. Reported next to the results so a
/// clean run can be told apart from a run that never reached deep states.
static CALLS: [[AtomicU64; 2]; 8] = [const { [AtomicU64::new(0), AtomicU64::new(0)] }; 8];

/// Reset the call counters.
pub fn reset_coverage() {
	CALLS.iter().flatten().for_each(|c| c.store(0, Ordering::Relaxed));
}

/// One line per call kind: how many succeeded and how many were rejected.
pub fn coverage() -> String {
	CALL_NAMES
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

/// Where a target keeps escrowed funds. It decides only when the payer can afford an escrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Custody {
	/// On hold on the payer's account.
	Hold,
	/// Moved into an account of the escrow's own.
	Account,
}

/// Limits the spec is checked against. They mirror each target's pallet configuration.
#[derive(Clone, Copy, Debug)]
pub struct SpecParams {
	pub min_milestone: Balance,
	pub deposit: Balance,
	pub max_milestones: usize,
	pub existential_deposit: Balance,
	pub max_balance: Balance,
	pub custody: Custody,
	/// Whether the target has `submit`, `dispute`, `claim` and `resolve`.
	pub claims: bool,
	pub challenge_period: u64,
}

/// Returned by a target for a `create` whose milestones exceed the bound: such a call never
/// decodes, so it cannot reach the pallet.
pub const UNDECODABLE: &str = "call does not decode: too many milestones";

/// A claim on an escrow's next milestone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
	pub submitted: u64,
	pub disputed: bool,
}

/// An escrow as the spec sees it. Targets report their storage in this shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
	pub payer: u64,
	pub beneficiary: u64,
	pub arbiter: Option<u64>,
	/// `(amount, deadline)`.
	pub milestones: Vec<(Balance, u64)>,
	pub released: u32,
	pub remaining: Balance,
	pub deposit: Balance,
	pub claim: Option<Claim>,
}

impl Record {
	fn next(&self) -> (Balance, u64) {
		self.milestones[self.released as usize]
	}
}

/// The pallet's events, as the spec sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecEvent {
	Created { id: u32, payer: u64, beneficiary: u64, total: Balance },
	MilestoneReleased { id: u32, index: u32, amount: Balance },
	Completed { id: u32 },
	Refunded { id: u32, amount: Balance },
	Cancelled { id: u32, amount: Balance },
	Submitted { id: u32, index: u32 },
	Disputed { id: u32, index: u32 },
	Resolved { id: u32, index: u32, to_beneficiary: Balance, to_payer: Balance },
}

/// A pallet under test, wrapped in its own mock runtime.
pub trait Target {
	const NAME: &'static str;
	const PARAMS: SpecParams;

	fn new_ext() -> sp_io::TestExternalities;
	/// Milestones are `(amount, deadline)`. A classic target takes `deadline` and the amounts.
	fn create(
		payer: u64,
		beneficiary: u64,
		arbiter: Option<u64>,
		milestones: Vec<(Balance, u64)>,
		deadline: u64,
	) -> DispatchResult;
	fn release(who: u64, id: u32) -> DispatchResult;
	fn refund(who: u64, id: u32) -> DispatchResult;
	fn cancel(who: u64, id: u32) -> DispatchResult;
	fn submit(_who: u64, _id: u32) -> DispatchResult {
		unreachable!("{} has no claims", Self::NAME)
	}
	fn dispute(_who: u64, _id: u32) -> DispatchResult {
		unreachable!("{} has no claims", Self::NAME)
	}
	fn claim(_who: u64, _id: u32) -> DispatchResult {
		unreachable!("{} has no claims", Self::NAME)
	}
	fn resolve(_who: u64, _id: u32, _to_beneficiary: Balance) -> DispatchResult {
		unreachable!("{} has no claims", Self::NAME)
	}
	fn block() -> u64;
	fn set_block(n: u64);
	fn free(who: u64) -> Balance;
	/// Funds escrowed on `who`'s behalf, wherever the target keeps them.
	fn escrowed(who: u64) -> Balance;
	/// Storage deposits paid by `who` for live escrows.
	fn deposits(who: u64) -> Balance;
	/// The ledger's `(frozen, reserved)` for `who`, which bound what it can escrow.
	fn ledger(who: u64) -> (Balance, Balance);
	fn total_issuance() -> Balance;
	fn next_id() -> u32;
	fn escrows() -> BTreeMap<u32, Record>;
	/// Escrow events since the last [`Target::reset_events`], or `None` if this target's events
	/// are not held to the spec.
	fn events() -> Option<Vec<SpecEvent>>;
	fn reset_events();
	/// Set or replace the one lock another pallet keeps on `who`.
	fn lock(who: u64, amount: Balance);
	fn unlock(who: u64);
	/// A plain transfer outside the escrow pallet. It may fail; the spec does not care.
	fn transfer(from: u64, to: u64, amount: Balance);
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
	DeadlinesOutOfOrder,
	DeadlineNotReached,
	Overflow,
	/// The payer cannot fund the escrow without going below what the ledger requires them to
	/// keep.
	Funds,
	NoArbiter,
	ClaimPending,
	DeadlinePassed,
	NoClaim,
	AlreadyDisputed,
	ChallengeOver,
	Disputed,
	ChallengeNotOver,
	NotDisputed,
	InvalidSplit,
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
			Reason::DeadlinesOutOfOrder => "deadlines out of order".into(),
			Reason::DeadlineNotReached => "payer refunded before the deadline".into(),
			Reason::Overflow => "milestone total overflows".into(),
			Reason::Funds => "payer cannot afford it".into(),
			Reason::NoArbiter => "no arbiter to settle a dispute".into(),
			Reason::ClaimPending => "a claim is pending".into(),
			Reason::DeadlinePassed => "the milestone's deadline has passed".into(),
			Reason::NoClaim => "nothing was submitted".into(),
			Reason::AlreadyDisputed => "the claim is already disputed".into(),
			Reason::ChallengeOver => "the challenge period is over".into(),
			Reason::Disputed => "the claim is disputed".into(),
			Reason::ChallengeNotOver => "the challenge period is still running".into(),
			Reason::NotDisputed => "nothing is disputed".into(),
			Reason::InvalidSplit => "the split is invalid".into(),
		}
	}

	/// The reason a dispatch error stands for. The pallets name their errors the same way.
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
				"DeadlinesOutOfOrder" => Some(Reason::DeadlinesOutOfOrder),
				"DeadlineNotReached" => Some(Reason::DeadlineNotReached),
				"Overflow" => Some(Reason::Overflow),
				"NoArbiter" => Some(Reason::NoArbiter),
				"ClaimPending" => Some(Reason::ClaimPending),
				"DeadlinePassed" => Some(Reason::DeadlinePassed),
				"NoClaim" => Some(Reason::NoClaim),
				"AlreadyDisputed" => Some(Reason::AlreadyDisputed),
				"ChallengeOver" => Some(Reason::ChallengeOver),
				"Disputed" => Some(Reason::Disputed),
				"ChallengeNotOver" => Some(Reason::ChallengeNotOver),
				"NotDisputed" => Some(Reason::NotDisputed),
				"InvalidSplit" => Some(Reason::InvalidSplit),
				_ => None,
			},
			DispatchError::Token(_) => Some(Reason::Funds),
			// The ledger checks an amount against total issuance before the payer's balance.
			DispatchError::Arithmetic(ArithmeticError::Underflow) => Some(Reason::Funds),
			DispatchError::Other(UNDECODABLE) => Some(Reason::TooManyMilestones),
			_ => None,
		}
	}
}

/// A milestone: usually a realistic amount, about 3% of the time close to the target's maximum
/// balance so that overflow is reachable without drowning every sequence in it.
#[derive(Arbitrary, Clone, Copy, Debug)]
pub struct Amount {
	pub value: u16,
	pub huge: u8,
	/// Blocks between this milestone's deadline and the previous one's, for targets with a
	/// deadline per milestone.
	pub gap: u8,
}

impl Amount {
	fn value(self, well_formed: bool, p: SpecParams) -> Balance {
		if self.huge < 8 {
			p.max_balance - Balance::from(self.value)
		} else if well_formed {
			p.min_milestone + Balance::from(self.value) % 1_500
		} else {
			Balance::from(self.value % 3_000)
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
	Submit {
		who: u8,
		id: u8,
	},
	Dispute {
		who: u8,
		id: u8,
	},
	Claim {
		who: u8,
		id: u8,
	},
	Resolve {
		who: u8,
		id: u8,
		/// Nothing, everything, or a share that may be invalid.
		split: u8,
		share: u16,
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

#[derive(Clone, Copy)]
enum Role {
	Payer,
	Beneficiary,
	Arbiter,
}

/// Pick a caller by role in the escrow, most often the role the call is meant for, so that
/// authorised and unauthorised calls both happen often.
fn caller(who: u8, parties: Option<&(u64, u64, Option<u64>)>, role: Role) -> u64 {
	let Some(&(payer, beneficiary, arbiter)) = parties else {
		return account(who);
	};
	let third = arbiter
		.unwrap_or_else(|| (1..=ACCOUNTS).find(|a| *a != payer && *a != beneficiary).unwrap_or(1));
	let preferred = match role {
		Role::Payer => payer,
		Role::Beneficiary => beneficiary,
		Role::Arbiter => third,
	};
	match who % 8 {
		0..=2 => preferred,
		3 => payer,
		4 => beneficiary,
		5 => third,
		_ => account(who / 8),
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
			Action::Submit { .. } => "submit",
			Action::Dispute { .. } => "dispute",
			Action::Claim { .. } => "claim",
			Action::Resolve { .. } => "resolve",
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

/// Per-account `(free, escrowed, deposits)`.
type Balances = BTreeMap<u64, (Balance, Balance, Balance)>;

/// Everything a call can change that the spec has an opinion on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
	balances: Balances,
	/// Per-account `(frozen, reserved)`: not the spec's business, except that they bound what
	/// an account can escrow.
	ledger: BTreeMap<u64, (Balance, Balance)>,
	escrows: BTreeMap<u32, Record>,
	next_id: u32,
	events: Option<Vec<SpecEvent>>,
}

fn snapshot<T: Target>() -> State {
	State {
		balances: (1..=ACCOUNTS)
			.map(|who| (who, (T::free(who), T::escrowed(who), T::deposits(who))))
			.collect(),
		ledger: (1..=ACCOUNTS).map(|who| (who, T::ledger(who))).collect(),
		escrows: T::escrows(),
		next_id: T::next_id(),
		events: T::events(),
	}
}

/// Amounts in the model are bounded by total issuance, far inside `i128`.
fn signed(amount: Balance) -> i128 {
	i128::try_from(amount).expect("model amounts are bounded by issuance")
}

/// Signed balance changes the spec expects from one successful call.
#[derive(Default)]
struct Effect(BTreeMap<u64, (i128, i128, i128)>);

impl Effect {
	fn add(&mut self, who: u64, free: i128, escrowed: i128, deposits: i128) {
		let e = self.0.entry(who).or_default();
		e.0 += free;
		e.1 += escrowed;
		e.2 += deposits;
	}

	fn apply(&self, before: &Balances) -> Option<Balances> {
		let shift = |value: Balance, by: i128| {
			i128::try_from(value).ok().and_then(|v| Balance::try_from(v + by).ok())
		};
		let mut out = before.clone();
		for (who, (free, escrowed, deposits)) in &self.0 {
			let row = out.get_mut(who)?;
			row.0 = shift(row.0, *free)?;
			row.1 = shift(row.1, *escrowed)?;
			row.2 = shift(row.2, *deposits)?;
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

/// Whether `who` can put `amount` into an escrow.
///
/// With [`Custody::Hold`] the ledger only asks that the free balance stay at or above the
/// existential deposit: a lock covers held funds as well as free ones, so holding locked funds
/// is allowed. With [`Custody::Account`] the funds leave the account, so the free balance that
/// stays behind must also cover whatever part of the locks the account's holds do not.
fn affordable(before: &State, who: u64, amount: Balance, p: SpecParams) -> bool {
	let (free, ..) = before.balances[&who];
	let (frozen, reserved) = before.ledger[&who];
	let keep = match p.custody {
		Custody::Hold => p.existential_deposit,
		Custody::Account => p.existential_deposit.max(frozen.saturating_sub(reserved)),
	};
	amount.checked_add(keep).is_some_and(|need| free >= need)
}

impl Model {
	fn from_storage<T: Target>() -> Self {
		let escrows = T::escrows();
		let parties =
			escrows.iter().map(|(id, e)| (*id, (e.payer, e.beneficiary, e.arbiter))).collect();
		Model { escrows, next_id: T::next_id(), parties }
	}

	/// What a call must do, or every reason the spec gives for rejecting it.
	fn expect(
		&self,
		call: &Call,
		block: u64,
		p: SpecParams,
		before: &State,
	) -> Result<Expected, Vec<Reason>> {
		let mut out = Expected::default();
		let mut why = Vec::new();
		match call {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
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
				if milestones.iter().any(|m| m.0 < p.min_milestone) {
					why.push(Reason::MilestoneTooSmall);
				}
				if milestones.first().map_or(*deadline, |m| m.1) <= block {
					why.push(Reason::DeadlineInPast);
				}
				if milestones.windows(2).any(|w| w[0].1 > w[1].1) {
					why.push(Reason::DeadlinesOutOfOrder);
				}
				let fits = |amount: &Balance| *amount <= p.max_balance;
				let total = milestones
					.iter()
					.try_fold(0, |acc: Balance, m| acc.checked_add(m.0))
					.filter(fits);
				// Milestones that fit with a deposit that does not: no one can afford that, and
				// a pallet may say either.
				match total.map(|t| t.checked_add(p.deposit).filter(fits)) {
					None => why.push(Reason::Overflow),
					Some(None) => why.extend([Reason::Overflow, Reason::Funds]),
					Some(Some(funding)) if !affordable(before, *payer, funding, p) => {
						why.push(Reason::Funds)
					},
					Some(Some(_)) => {},
				}
				if !why.is_empty() {
					return Err(why);
				}
				let total = total.expect("checked above");
				let (t, d) = (signed(total), signed(p.deposit));
				out.effect.add(*payer, -(t + d), t, d);
				out.events.push(SpecEvent::Created {
					id: self.next_id,
					payer: *payer,
					beneficiary: *beneficiary,
					total,
				});
				return Ok(out);
			},
			Call::Release { id, .. }
			| Call::Refund { id, .. }
			| Call::Cancel { id, .. }
			| Call::Submit { id, .. }
			| Call::Dispute { id, .. }
			| Call::Claim { id, .. }
			| Call::Resolve { id, .. }
				if !self.escrows.contains_key(id) =>
			{
				return Err(vec![Reason::NotFound]);
			},
			_ => {},
		}

		let (id, who) = call.target();
		let e = &self.escrows[&id];
		let is = |party: Option<u64>| party == Some(who);
		let (amount, deadline) = e.next();
		let challenge_end = e.claim.map(|c| c.submitted + p.challenge_period);
		match call {
			Call::Create { .. } => unreachable!("handled above"),
			Call::Release { .. } => {
				if !is(Some(e.payer)) && !is(e.arbiter) {
					why.push(Reason::NotAuthorized);
				}
			},
			Call::Refund { .. } if !is(e.arbiter) => {
				if !is(Some(e.payer)) {
					why.push(Reason::NotAuthorized);
				} else {
					if block < deadline {
						why.push(Reason::DeadlineNotReached);
					}
					if e.claim.is_some() {
						why.push(Reason::ClaimPending);
					}
				}
			},
			Call::Refund { .. } => {},
			Call::Cancel { .. } => {
				if !is(Some(e.beneficiary)) {
					why.push(Reason::NotAuthorized);
				}
			},
			Call::Submit { .. } => {
				if !is(Some(e.beneficiary)) {
					why.push(Reason::NotAuthorized);
				}
				if e.arbiter.is_none() {
					why.push(Reason::NoArbiter);
				}
				if e.claim.is_some() {
					why.push(Reason::ClaimPending);
				}
				if block >= deadline {
					why.push(Reason::DeadlinePassed);
				}
			},
			Call::Dispute { .. } => {
				if !is(Some(e.payer)) {
					why.push(Reason::NotAuthorized);
				}
				match e.claim {
					None => why.push(Reason::NoClaim),
					Some(claim) => {
						if claim.disputed {
							why.push(Reason::AlreadyDisputed);
						}
						if challenge_end.is_some_and(|end| block >= end) {
							why.push(Reason::ChallengeOver);
						}
					},
				}
			},
			Call::Claim { .. } => {
				if !is(Some(e.beneficiary)) {
					why.push(Reason::NotAuthorized);
				}
				match e.claim {
					None => why.push(Reason::NoClaim),
					Some(claim) => {
						if claim.disputed {
							why.push(Reason::Disputed);
						}
						if challenge_end.is_some_and(|end| block < end) {
							why.push(Reason::ChallengeNotOver);
						}
					},
				}
			},
			Call::Resolve { to_beneficiary, .. } => {
				if !is(e.arbiter) {
					why.push(Reason::NotAuthorized);
				}
				if !e.claim.is_some_and(|c| c.disputed) {
					why.push(Reason::NotDisputed);
				}
				let valid = |share: Balance| share == 0 || share >= p.min_milestone;
				if amount
					.checked_sub(*to_beneficiary)
					.is_none_or(|to_payer| !valid(*to_beneficiary) || !valid(to_payer))
				{
					why.push(Reason::InvalidSplit);
				}
			},
		}
		if !why.is_empty() {
			return Err(why);
		}

		let index = e.released;
		let last = index as usize + 1 == e.milestones.len();
		let pay = |out: &mut Expected, to_beneficiary: Balance| {
			let to_payer = amount - to_beneficiary;
			out.effect.add(e.payer, signed(to_payer), -signed(amount), 0);
			out.effect.add(e.beneficiary, signed(to_beneficiary), 0, 0);
			if last {
				let deposit = signed(e.deposit);
				out.effect.add(e.payer, deposit, 0, -deposit);
			}
		};
		match call {
			Call::Release { .. } | Call::Claim { .. } => {
				pay(&mut out, amount);
				out.events.push(SpecEvent::MilestoneReleased { id, index, amount });
			},
			Call::Resolve { to_beneficiary, .. } => {
				pay(&mut out, *to_beneficiary);
				out.events.push(SpecEvent::Resolved {
					id,
					index,
					to_beneficiary: *to_beneficiary,
					to_payer: amount - to_beneficiary,
				});
			},
			Call::Refund { .. } | Call::Cancel { .. } => {
				let (remaining, deposit) = (signed(e.remaining), signed(e.deposit));
				out.effect.add(e.payer, remaining + deposit, -remaining, -deposit);
				out.events.push(if matches!(call, Call::Refund { .. }) {
					SpecEvent::Refunded { id, amount: e.remaining }
				} else {
					SpecEvent::Cancelled { id, amount: e.remaining }
				});
				return Ok(out);
			},
			Call::Submit { .. } => out.events.push(SpecEvent::Submitted { id, index }),
			Call::Dispute { .. } => out.events.push(SpecEvent::Disputed { id, index }),
			Call::Create { .. } => unreachable!("handled above"),
		}
		if last && matches!(call, Call::Release { .. } | Call::Claim { .. } | Call::Resolve { .. })
		{
			out.events.push(SpecEvent::Completed { id });
		}
		Ok(out)
	}

	/// Record a successful call. Only called once the effect has been checked.
	fn commit(&mut self, call: &Call, block: u64, p: SpecParams) {
		if let Call::Create { payer, beneficiary, arbiter, milestones, .. } = call {
			self.escrows.insert(
				self.next_id,
				Record {
					payer: *payer,
					beneficiary: *beneficiary,
					arbiter: *arbiter,
					milestones: milestones.clone(),
					released: 0,
					remaining: milestones.iter().map(|m| m.0).sum(),
					deposit: p.deposit,
					claim: None,
				},
			);
			self.parties.insert(self.next_id, (*payer, *beneficiary, *arbiter));
			self.next_id += 1;
			return;
		}
		let (id, _) = call.target();
		let e = self.escrows.get_mut(&id).expect("checked in expect");
		match call {
			Call::Release { .. } | Call::Claim { .. } | Call::Resolve { .. } => {
				e.remaining -= e.next().0;
				e.released += 1;
				e.claim = None;
				if e.released as usize == e.milestones.len() {
					self.escrows.remove(&id);
				}
			},
			Call::Refund { .. } | Call::Cancel { .. } => {
				self.escrows.remove(&id);
			},
			Call::Submit { .. } => e.claim = Some(Claim { submitted: block, disputed: false }),
			Call::Dispute { .. } => e.claim.as_mut().expect("checked in expect").disputed = true,
			Call::Create { .. } => unreachable!("handled above"),
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
		milestones: Vec<(Balance, u64)>,
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
	Submit {
		who: u64,
		id: u32,
	},
	Dispute {
		who: u64,
		id: u32,
	},
	Claim {
		who: u64,
		id: u32,
	},
	Resolve {
		who: u64,
		id: u32,
		to_beneficiary: Balance,
	},
}

impl Call {
	/// The escrow and caller of any call but `Create`.
	fn target(&self) -> (u32, u64) {
		match *self {
			Call::Release { who, id }
			| Call::Refund { who, id }
			| Call::Cancel { who, id }
			| Call::Submit { who, id }
			| Call::Dispute { who, id }
			| Call::Claim { who, id }
			| Call::Resolve { who, id, .. } => (id, who),
			Call::Create { .. } => unreachable!("create has no escrow yet"),
		}
	}

	fn index(&self) -> usize {
		match self {
			Call::Create { .. } => 0,
			Call::Release { .. } => 1,
			Call::Refund { .. } => 2,
			Call::Cancel { .. } => 3,
			Call::Submit { .. } => 4,
			Call::Dispute { .. } => 5,
			Call::Claim { .. } => 6,
			Call::Resolve { .. } => 7,
		}
	}

	fn dispatch<T: Target>(&self) -> DispatchResult {
		match self {
			Call::Create { payer, beneficiary, arbiter, milestones, deadline } => {
				T::create(*payer, *beneficiary, *arbiter, milestones.clone(), *deadline)
			},
			Call::Release { who, id } => T::release(*who, *id),
			Call::Refund { who, id } => T::refund(*who, *id),
			Call::Cancel { who, id } => T::cancel(*who, *id),
			Call::Submit { who, id } => T::submit(*who, *id),
			Call::Dispute { who, id } => T::dispute(*who, *id),
			Call::Claim { who, id } => T::claim(*who, *id),
			Call::Resolve { who, id, to_beneficiary } => T::resolve(*who, *id, *to_beneficiary),
		}
	}
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

/// Turn an action into a call against the model's current state, or perform it directly if it
/// is not a pallet call. `None` means there is nothing to check.
fn resolve_action<T: Target>(action: &Action, model: &Model, block: u64) -> Option<Call> {
	let p = T::PARAMS;
	let span = model.next_id + 1;
	// Three in four calls go to a live escrow where the call can matter (a claim to dispute, a
	// dispute to resolve), so that claims get that far; the rest to any id, closed and
	// never-created ones included.
	let pick = |who: u8, id: u8, role, fits: fn(&Record) -> bool| {
		let live: Vec<u32> = model.escrows.keys().copied().collect();
		let fitting: Vec<u32> =
			live.iter().copied().filter(|id| fits(&model.escrows[id])).collect();
		let from = if fitting.is_empty() { &live } else { &fitting };
		let id = match id % 4 {
			0 => u32::from(id) % span,
			_ if from.is_empty() => u32::from(id) % span,
			_ => from[usize::from(id / 4) % from.len()],
		};
		(caller(who, model.parties.get(&id), role), id)
	};
	let any = |_: &Record| true;
	let claimed = |e: &Record| e.claim.is_some();
	Some(match action.clone() {
		// Short steps: deadlines are at most a few dozen blocks out and challenge periods shorter.
		Action::Advance { blocks } => {
			T::set_block(block + u64::from(blocks % 6));
			return None;
		},
		Action::Lock { who, amount } => {
			T::lock(account(who), Balance::from(amount) % (2 * Balance::from(START_BALANCE)));
			return None;
		},
		Action::Unlock { who } => {
			T::unlock(account(who));
			return None;
		},
		Action::Spend { who, to, amount, mode } => {
			let (from, free) = (account(who), T::free(account(who)));
			let amount = match mode % 3 {
				0 => Balance::from(amount) % (free + 1),
				1 => free.saturating_sub(p.existential_deposit),
				_ => free,
			};
			T::transfer(from, other_account(from, to), amount);
			return None;
		},
		Action::Submit { .. }
		| Action::Dispute { .. }
		| Action::Claim { .. }
		| Action::Resolve { .. }
			if !p.claims =>
		{
			return None;
		},
		Action::Create { payer, beneficiary, arbiter, milestones, deadline_in, shape } => {
			let well_formed = shape % 4 != 0;
			let payer = account(payer);
			let generated = milestones.iter().take(MAX_MILESTONES_GENERATED);
			if well_formed {
				let beneficiary = other_account(payer, beneficiary);
				let deadline = block + 1 + u64::from(deadline_in % 20);
				let mut at = deadline;
				let mut amounts: Vec<(Balance, u64)> = generated
					.map(|m| {
						if p.claims {
							at += u64::from(m.gap % 8);
						}
						(m.value(true, p), at)
					})
					.collect();
				amounts.truncate(p.max_milestones);
				if amounts.is_empty() {
					amounts.push((p.min_milestone, deadline));
				}
				let third_parties: Vec<u64> =
					(1..=ACCOUNTS).filter(|x| *x != payer && *x != beneficiary).collect();
				Call::Create {
					payer,
					beneficiary,
					arbiter: arbiter.map(|a| third_parties[usize::from(a) % third_parties.len()]),
					deadline: amounts[0].1,
					milestones: amounts,
				}
			} else {
				// Spans a little into the past so past deadlines are exercised too.
				let deadline = (block + u64::from(deadline_in % 24)).saturating_sub(2);
				let amounts = generated
					.map(|m| {
						let at = if p.claims {
							(deadline + u64::from(m.gap % 12)).saturating_sub(4)
						} else {
							deadline
						};
						(m.value(false, p), at)
					})
					.collect();
				Call::Create {
					payer,
					beneficiary: account(beneficiary),
					arbiter: arbiter.map(account),
					milestones: amounts,
					deadline,
				}
			}
		},
		Action::Release { who, id } => {
			let (who, id) = pick(who, id, Role::Payer, any);
			Call::Release { who, id }
		},
		Action::Refund { who, id } => {
			let (who, id) = pick(who, id, Role::Payer, any);
			Call::Refund { who, id }
		},
		Action::Cancel { who, id } => {
			let (who, id) = pick(who, id, Role::Beneficiary, any);
			Call::Cancel { who, id }
		},
		Action::Submit { who, id } => {
			let (who, id) =
				pick(who, id, Role::Beneficiary, |e| e.arbiter.is_some() && e.claim.is_none());
			Call::Submit { who, id }
		},
		Action::Dispute { who, id } => {
			let (who, id) = pick(who, id, Role::Payer, claimed);
			Call::Dispute { who, id }
		},
		Action::Claim { who, id } => {
			let (who, id) = pick(who, id, Role::Beneficiary, claimed);
			Call::Claim { who, id }
		},
		Action::Resolve { who, id, split, share } => {
			let (who, id) = pick(who, id, Role::Arbiter, |e| e.claim.is_some_and(|c| c.disputed));
			let amount = model.escrows.get(&id).map_or(Balance::from(share), |e| e.next().0);
			let to_beneficiary = match split % 3 {
				0 => 0,
				1 => amount,
				_ => Balance::from(share) % (amount + 1),
			};
			Call::Resolve { who, id, to_beneficiary }
		},
	})
}

/// Run one call sequence against a fresh chain and return the first spec violation, if any.
pub fn run<T: Target>(actions: &[Action]) -> Result<(), Violation> {
	run_in::<T>(&mut T::new_ext(), actions)
}

/// Run a call sequence against a chain in any state. The model takes the escrows already stored
/// as given; everything from here on is held to the spec.
pub fn run_in<T: Target>(
	ext: &mut sp_io::TestExternalities,
	actions: &[Action],
) -> Result<(), Violation> {
	ext.execute_with(|| {
		let issuance = T::total_issuance();
		let mut model = Model::from_storage::<T>();

		for (step, action) in actions.iter().take(MAX_ACTIONS).enumerate() {
			let fail =
				|kind, detail: String| Violation { step, action: action.clone(), kind, detail };
			let block = T::block();
			let Some(call) = resolve_action::<T>(action, &model, block) else {
				continue;
			};

			T::reset_events();
			let before = snapshot::<T>();
			IN_CALL.with(|c| c.set(true));
			let outcome = catch_unwind(AssertUnwindSafe(|| call.dispatch::<T>()));
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
			CALLS[call.index()][usize::from(outcome.is_err())].fetch_add(1, Ordering::Relaxed);

			match (outcome, model.expect(&call, block, T::PARAMS, &before)) {
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
					model.commit(&call, block, T::PARAMS);
					if after.next_id != model.next_id {
						return Err(fail(
							Kind::IdMismatch,
							format!("pallet next id {} model {}", after.next_id, model.next_id),
						));
					}
					if after.escrows != model.escrows {
						let left = after.escrows.keys().any(|id| !model.escrows.contains_key(id));
						return Err(fail(
							if left { Kind::ClosedEscrowLeft } else { Kind::StorageMismatch },
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
