# Threat model: pallet-escrow

## What the pallet does

A payer locks funds for a beneficiary, split into milestones. The payer (or an optional arbiter) releases milestones one at a time. After a deadline the payer can take back whatever is left. The arbiter can refund at any time. The beneficiary can walk away (cancel) at any time, which also returns the rest to the payer.

Funds never leave the payer's account until they are paid out: they sit in a `pallet-balances` hold under `HoldReason::Escrow`, next to a storage deposit under `HoldReason::StorageDeposit`.

## Assets

| Asset | Why it matters |
|---|---|
| Escrowed funds (payer's `Escrow` hold) | Direct loss if released to the wrong account, released twice, or unlocked early |
| Storage deposits | Pay for state; must come back exactly once |
| Block weight and proof size | Mis-weighted or panicking calls give attackers free block space or stall block production |
| State size | Unbounded or free-to-create entries grow every node's database at the attacker's leisure |

## Actors and trust

| Actor | Trusted for | Must not be able to |
|---|---|---|
| Payer | Deciding when a milestone is earned | Take funds back before the deadline; affect other payers' escrows |
| Beneficiary | Nothing | Pull funds early; affect the payer's other escrows |
| Arbiter | Resolving disputes: release or refund | **Receive funds.** An arbiter decides where money goes, never where it ends up for themselves |
| Any other signed account | Nothing | Change any escrow |
| Block author | Ordering transactions | Break accounting by ordering (all calls are independent and transactional) |
| Governance (runtime config) | Setting `MinMilestone`, `EscrowDeposit`, `MaxMilestones` | Set a configuration the pallet cannot run under (`integrity_test` enforces `MinMilestone >= ED` and `MaxMilestones > 0`) |

## Entry points and threats

| Entry point | Threat | Control in the hardened pallet |
|---|---|---|
| `create` | Integer overflow when totalling milestones (wraps in release builds) | `checked_add`, `Error::Overflow` |
| `create` | Free or near-free state growth | `EscrowDeposit` hold, `MinMilestone`, `BoundedVec<_, MaxMilestones>`, benchmarked-shape weight |
| `create` | Self-escrow or arbiter who is also a party | Rejected (`SelfEscrow`, `ArbiterConflict`) |
| `create` | Deadline already passed, so the payer can refund immediately | `DeadlineInPast` |
| `release` | Any account triggers payouts | Payer or arbiter only (`NotAuthorized`) |
| `release` | Panic on unknown id or past the last milestone | `ok_or(NotFound)`, `get()` + `Inconsistent`; the escrow is deleted on completion |
| `release` | Transfer more than this escrow holds, drawing on the payer's other escrows | `remaining` checked before transfer; `Precision::Exact` |
| `refund` | Arbiter refunds to themselves | Refund always goes to the payer |
| `refund` | Payer refunds before the deadline | Deadline enforced unless the caller is the arbiter |
| `cancel` | Cancelled escrow stays live and can be cancelled or released again | Escrow removed in the same transaction as the release |
| all | Shared hold drained by a stale or double release | Single `close()` path: exact release, then delete |
| all | Partial state change when the second of two holds fails | FRAME dispatchables are transactional; covered by `create_without_funds_changes_nothing` |

## Accepted risks

- **The arbiter is trusted to be fair.** It can refund at any time. The design limits the damage to "money goes back to the payer", never "money goes to the arbiter".
- **The payer can stall.** A payer who never releases waits out the deadline and refunds. That is the escrow's contract; an arbiter is the remedy.
- **Weights are hand-set upper bounds.** The benchmarks run in CI (`--features runtime-benchmarks`) but have not been run on reference hardware. `weights.rs` says so.
- **No per-account cap on escrows.** Spam is priced by the deposit instead. A runtime that wants a hard cap should add a counter.

## How this is verified

1. Unit tests, one per threat row above (`pallets/escrow/src/tests.rs`).
2. `try_state`: the hold balances must equal the sum over live escrows after every test.
3. The spec-oracle harness (`harness/`): random call sequences where every successful call must be permitted by the spec and move exactly the spec's amounts, every failed call must change nothing, nothing may panic, and total issuance never moves. The same driver runs under libFuzzer (`fuzz/`).
4. Static analysis with [frame-sentinel](https://github.com/sonofnos/frame-sentinel): zero findings on the hardened pallet.
