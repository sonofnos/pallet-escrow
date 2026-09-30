# Threat model: pallet-escrow

## What the pallet does

A payer funds an escrow for a beneficiary, split into milestones, each with its own deadline. The payer (or an optional arbiter) can release the next milestone at any time. With an arbiter named, the beneficiary does not have to wait for that: they `submit` the milestone as done, and unless the payer disputes it within `ChallengePeriod` blocks they `claim` it themselves. A disputed milestone waits for the arbiter to `resolve` it, splitting it between the two sides. Once a milestone's deadline passes with nothing submitted, the payer can take back what is left. The arbiter can refund at any time; the beneficiary can walk away (`cancel`) at any time, which returns the rest to the payer.

Each escrow's funds and storage deposit sit in an account of the escrow's own, derived from `PalletId` and the escrow id. Only this pallet can move them.

The escrow's accounting invariant, checked by `try_state` for every live escrow:

```text
balance(escrow account) >= remaining + deposit
remaining               == sum of unpaid milestones
```

`>=` because anyone can send funds to an escrow account; they go to the payer when it closes.

## Assets

| Asset | Why it matters |
|---|---|
| Escrowed funds (each escrow's account) | Direct loss if paid to the wrong account, paid twice, or returned early |
| The beneficiary's payout | Delivered work must be payable: a payout that can be blocked is a loss for the beneficiary even if no funds move |
| Storage deposits | Pay for state; must come back exactly once |
| Block weight and proof size | Mis-weighted or panicking calls give attackers free block space or stall block production |
| State size | Unbounded or free-to-create entries grow every node's database at the attacker's leisure |

## Actors and trust

| Actor | Trusted for | Must not be able to |
|---|---|---|
| Payer | Deciding when a milestone is earned; disputing a submitted one | Take funds back before the next deadline or while a claim is pending; block a payout; affect other escrows |
| Beneficiary | Nothing | Collect a milestone the payer disputed, or before the challenge period ends; affect other escrows |
| Arbiter | Deciding disputes: release, refund, or split a disputed milestone | **Receive funds.** The arbiter has decision authority, never custody |
| Other pallets acting on the payer's account (locks, freezes, holds) | Their own business | Block a payout or refund (ESC-11) |
| Any other signed account | Nothing | Change any escrow. Sending funds to an escrow account only gifts the payer |
| Block author | Ordering transactions | Break accounting by ordering (all calls are independent and transactional) |
| Governance (runtime config) | `MinMilestone`, `EscrowDeposit`, `MaxMilestones`, `ChallengePeriod`, `PalletId` | Set a configuration the pallet cannot run under: `integrity_test` enforces `MinMilestone >= ED`, `EscrowDeposit >= ED`, non-zero `MaxMilestones` and `ChallengePeriod`, and an `AccountId` long enough that escrow accounts never collide |

## Entry points and threats

| Entry point | Threat | Control |
|---|---|---|
| `create` | Integer overflow when totalling milestones, or adding the deposit (wraps in release builds) | `checked_add`, `Error::Overflow` |
| `create` | Free or near-free state growth | Deposit paid into the escrow account, `MinMilestone`, `BoundedVec<_, MaxMilestones>`, weight by milestone count |
| `create` | Self-escrow or arbiter who is also a party | Rejected (`SelfEscrow`, `ArbiterConflict`) |
| `create` | First deadline already passed, so the payer can refund immediately; deadlines out of order | `DeadlineInPast`, `DeadlinesOutOfOrder` |
| `create` | Escrowing funds already locked for something else | The transfer into the escrow account respects the payer's locks |
| `release`, `claim` | Any account triggers payouts | Payer or arbiter releases; only the beneficiary claims |
| `release`, `claim`, `resolve` | Pay more than this escrow holds, drawing on other escrows | Each escrow pays from its own account; the ledger refuses to overdraw it |
| `release`, `claim`, `resolve` | A lock on the payer blocks the payout (ESC-11) | The funds left the payer's account at `create` |
| `release`, `claim`, `resolve` | Panic on an unknown id or past the last milestone | `NotFound`, `Inconsistent`; the escrow is deleted when its last milestone settles |
| `refund` | Arbiter refunds to themselves | Refunds always go to the payer |
| `refund` | Payer refunds early, or stalls until the deadline and refunds work already delivered | The next milestone's deadline must have passed and no claim may be pending; the beneficiary submits before the deadline |
| `cancel` | Cancelled escrow stays live and can be cancelled or released again | Escrow removed in the same transaction as the payout |
| `submit` | A claim no one can settle | Only with an arbiter (`NoArbiter`); only before the milestone's deadline; one at a time |
| `dispute` | Payer stalls a claim forever | Only the payer, once, within `ChallengePeriod` of the submission; after that the claim stands |
| `claim` | Beneficiary collects a disputed or unexamined milestone | Only undisputed, only after `ChallengePeriod` |
| `resolve` | Arbiter pays a share that cannot be credited, or more than the milestone | Shares are zero or at least `MinMilestone` (`>= ED`) and add up to the milestone (`InvalidSplit`) |
| all | Funds sent to an escrow account break the accounting | `try_state` checks `>=`; closing sends everything in the account to the payer |
| all | Partial state change when a later step fails | FRAME dispatchables are transactional |
| migration | Holds left behind, or funds stranded if an escrow cannot move | Held funds move with `Fortitude::Force` (the payer's locks cover them, ESC-11); a deposit below the ED is topped up from the payer; an escrow that still cannot move is refunded rather than dropped. Checked in `post_upgrade` |

## Accepted risks

- **The arbiter is trusted to decide fairly.** It can refund at any time and split any disputed milestone. The design limits the damage to "money goes back to the payer or on to the beneficiary", never "money goes to the arbiter".
- **An arbiter that never acts** leaves a disputed milestone waiting. The funds stay in escrow until the arbiter decides, the payer releases, or the beneficiary cancels. There is no dispute timeout.
- **Without an arbiter, the beneficiary trusts the payer.** No one can settle a dispute, so claims are unavailable: the payer releases, or refunds once a deadline passes.
- **No per-account cap on escrows.** Spam is priced by the deposit instead.
- **The migration runs in one block.** A chain with more escrows than fit in a block has to split it.

## How this is verified

1. Unit tests, one per threat row above (`pallets/escrow/src/tests.rs`), with `try_state` after each.
2. The spec-oracle harness (`harness/`): random call sequences against the pallet, its hold-based predecessor and the first draft, interleaved with what other pallets do (locks, transfers down to zero). Every call the spec requires must succeed; every rejection must carry a reason the spec gives and change nothing; every success must move exactly the spec's amounts, store exactly the spec's escrows and emit exactly the spec's events. Nothing may panic and total issuance never moves. The same driver runs under libFuzzer (`fuzz/`).
3. The migration, checked on state the hold-based pallet produced: nothing observable may change, no hold may remain, and the migrated escrows must then follow the spec (`harness/tests/migration.rs`).
4. Static analysis with [frame-sentinel](https://github.com/sonofnos/frame-sentinel).
