# Security review: pallet-escrow

A self-review: the author reviewing their own code as an auditor would, not an independent audit.

| | |
|---|---|
| Targets | `pallets/escrow-v0` (the first draft) and `pallets/escrow-v1` (the hold-based pallet that fixed it), both kept unchanged so every finding reproduces |
| Framework | Polkadot SDK stable2606 (`frame-support` 48, `pallet-balances` 50) |
| Methods | Manual review against the [threat model](THREAT_MODEL.md); static analysis with [frame-sentinel](https://github.com/sonofnos/frame-sentinel); spec-oracle harness driven by seeded sequences and libFuzzer (`harness/`, `fuzz/`) |
| Outcome | 11 findings: 2 critical, 4 high, 2 medium, 1 low, 2 informational. ESC-01 to ESC-10 were found in v0 and fixed in v1; ESC-11 was found in v1 and fixed there, then designed out in v2 (`pallets/escrow`). Each has a regression test |

## Summary

| Id | Severity | Title | Found by |
|---|---|---|---|
| ESC-01 | Critical | Arbiter refund is paid to the arbiter | review, harness |
| ESC-02 | Critical | Cancel leaves the escrow live; repeat calls drain the payer's other escrows | review, harness |
| ESC-03 | High | Milestone total wraps in release builds; payouts draw on other escrows | review, frame-sentinel, harness |
| ESC-04 | High | Any account can release milestones | review, harness |
| ESC-05 | High | Payer can refund before the deadline | review, harness |
| ESC-06 | Medium | Panics on an unknown id and after the final milestone | review, frame-sentinel, harness |
| ESC-07 | Medium | Zero weight, unbounded storage and no deposit: free block space and state growth | review, frame-sentinel |
| ESC-08 | Low | No input validation on `create` | review, harness |
| ESC-09 | Info | Unchecked `NextEscrowId` increment | review, frame-sentinel |
| ESC-10 | Info | `Precision::BestEffort` hides accounting errors | review |
| ESC-11 | High | A lock on the payer blocks every payout (v1); in v0 it silently underpays | harness |

Severity follows impact and likelihood. **Critical** means direct loss of user funds by an unprivileged or semi-trusted party. **High** means loss of funds needing collusion, or a broken core guarantee. **Medium** means liveness or cost problems. **Low** means incorrect behaviour without direct loss.

The harness column is not a claim made after the fact. `harness/tests/invariants.rs` runs 20,000 seeded sequences against v0 in CI and fails unless it rediscovers ESC-01 to ESC-06 on its own, and `harness/examples/poc.rs` reproduces the three fund-loss findings with balances printed. ESC-11 was found by the harness alone, after it learned to require that calls the spec owes the caller succeed, and to act as other pallets do on the same accounts. The numbers below are from those runs.

## Findings

### ESC-01 Critical: arbiter refund is paid to the arbiter

`refund` lets the payer or the arbiter call it, then transfers `remaining` from the payer's hold to **the caller**:

```rust
T::Currency::transfer_on_hold(&HoldReason::Escrow.into(), &escrow.payer, &who, escrow.remaining, ...)
```

The arbiter is trusted to decide where money goes, not to receive it. Any arbiter can take the full remaining balance of every escrow it arbitrates.

**Proof of concept** (`cargo run --release -p escrow-harness --example poc`):

```
payer escrows 1000 for honest, arbiter set   payer free   9000  payer held   1000  honest  10000  puppet  10000  arbiter  10000
arbiter calls refund                         payer free   9000  payer held      0  honest  10000  puppet  10000  arbiter  11000
```

**Fix:** a refund always releases to the payer (`close()`); the arbiter only decides that it happens. Test: `arbiter_refund_pays_the_payer_not_the_arbiter`.

### ESC-02 Critical: cancel leaves the escrow live

`cancel` releases `remaining` back to the payer but never removes the escrow. Holds are per `(account, reason)`, not per escrow, so a second `cancel` releases the same amount again out of the hold that backs the payer's **other** escrows. `BestEffort` precision (ESC-10) means it never fails, it just takes what is there. A payer who controls one beneficiary account can unlock every escrow they have funded, and honest beneficiaries' later "releases" succeed while paying nothing.

```
payer escrows 1000 for honest, 1000 for puppet payer free   8000  payer held   2000  honest  10000  puppet  10000  arbiter  10000
puppet cancels escrow 1 twice                payer free  10000  payer held      0  honest  10000  puppet  10000  arbiter  10000
payer 'releases' honest's escrow: Ok(())     payer free  10000  payer held      0  honest  10000  puppet  10000  arbiter  10000
```

The harness sees it at the root: after the first `cancel`, the escrow is still in storage although the spec says it is closed (`cancel: closed escrow left in storage`, 1,689 of 20,000 sequences). Without the storage comparison it surfaces later, as a second `cancel`, a `release` or a `refund` succeeding on a closed escrow.

**Fix:** one `close()` path releases this escrow's own funds with `Precision::Exact` and deletes it in the same transaction. Test: `cancel_is_final_and_leaves_other_escrows_funded`. v1 also counts each payer's live escrows, so closing the last one can check that nothing is left on hold, the direction `try_state` cannot see. v2 removes the shared hold altogether: each escrow has its own account.

### ESC-03 High: milestone total wraps in release builds

```rust
let total = milestones.iter().fold(BalanceOf::<T>::zero(), |acc, m| acc + *m);
```

Runtimes are compiled without overflow checks, so this wraps. `[u64::MAX - 5, 10]` holds 4 units against an 18-quintillion first milestone. `release` then transfers "as much as possible" (ESC-10) of that milestone, which is the whole shared hold, including other beneficiaries' funds:

```
payer escrows 1000 for honest                payer free   9000  payer held   1000  honest  10000  puppet  10000
payer opens [u64::MAX-5, 10] for puppet      payer free   8996  payer held   1004  honest  10000  puppet  10000
payer releases puppet's first milestone      payer free   8996  payer held      0  honest  10000  puppet  11004
payer releases honest's escrow: Ok(())       payer free   8996  payer held      0  honest  10000  puppet  11004
```

In a debug build the same input panics (the harness reports `create: arithmetic overflow panic`). Run as a runtime runs, it reports `create: succeeded but milestone total overflows`, and, when the wrapped total happens to exceed the payer's balance, `create: rejected for a reason the spec does not give`: the pallet says the payer cannot afford it where the spec says the total overflows. Both CI jobs are kept (`hunt-report`).

**Fix:** `checked_add` with `Error::Overflow`. Test: `milestone_overflow_is_rejected`.

### ESC-04 High: any account can release

`release` calls `ensure_signed(origin)?` and discards the account. Anyone can push every milestone to the beneficiary immediately, removing the payer's control over timing, which is the only reason the escrow exists. Harness: `release: succeeded but caller may not release`, 1,113 of 20,000 sequences.

**Fix:** payer or arbiter only. Test: `stranger_and_beneficiary_cannot_release`.

### ESC-05 High: payer can refund before the deadline

`refund` accepts the payer at any time. A beneficiary who delivers work has no guarantee: the payer can watch for delivery and pull the funds first. Harness: `refund: succeeded but payer refunded before the deadline`, 1,672 of 20,000 sequences.

**Fix:** the payer may refund only from `deadline`; the arbiter at any time. Test: `payer_refund_waits_for_deadline`.

### ESC-06 Medium: panics in dispatch

`Escrows::<T>::get(id).unwrap()` panics on an unknown id. Because a completed escrow is never deleted, `escrow.milestones[escrow.released as usize]` panics on the next `release`. A panicking extrinsic is dropped from the block without paying its fee, so anyone can make block authors do work for free. Harness: `release: unwrap on missing escrow panic` (first seed 0), and the completed escrow left behind, `release: closed escrow left in storage` (1,332 sequences), which the next `release` turns into an index-out-of-bounds panic. frame-sentinel: FS001 (high), FS008.

**Fix:** `ok_or(Error::NotFound)`, `.get()` with `Error::Inconsistent`, delete on completion. Test: `unknown_or_finished_escrow_is_an_error_not_a_panic`.

### ESC-07 Medium: zero weight, unbounded storage, no deposit

Every call is `#[pallet::weight(Weight::zero())]`, the pallet is `#[pallet::without_storage_info]`, milestones are an unbounded `Vec`, and creating an escrow costs no deposit. Calls are free block space and each escrow is permanent state at no cost. frame-sentinel: FS006 ×4, FS004.

**Fix:** `BoundedVec<_, MaxMilestones>`, `MaxEncodedLen` storage, an `EscrowDeposit` returned on close, and weights from `WeightInfo` with benchmarks covering every call. v1's weights were hand-set upper bounds; v2's are generated by `frame-omni-bencher` against `runtime/`, and showed that the hand-set proof sizes were about a third too low (6,196 bytes measured for `refund` against 4,000 declared). They were measured on a development machine, not reference hardware.

### ESC-08 Low: no input validation on `create`

Empty milestone lists, zero-value milestones, `payer == beneficiary`, an arbiter who is a party, and deadlines in the past are all accepted. None moves funds wrongly on its own, but each creates escrows whose guarantees are meaningless. The past-deadline case turns ESC-05 into an immediate refund even after the fix. Harness: each of the five, 1 to 1,035 sequences.

**Fix:** `NoMilestones`, `MilestoneTooSmall` (with `MinMilestone >= ED` checked in `integrity_test`), `SelfEscrow`, `ArbiterConflict`, `DeadlineInPast`. Test: `create_rejects_bad_input`.

### ESC-09 Info: unchecked id increment

`NextEscrowId::<T>::put(id + 1)` wraps at `u32::MAX` and would overwrite escrow 0. Unreachable in practice once deposits exist, but free to fix. **Fix:** `checked_add` with `IdsExhausted`.

### ESC-10 Info: `BestEffort` hides accounting errors

Every transfer and release uses `Precision::BestEffort`, so an attempt to move more than is held silently moves less instead of failing. That is what turns ESC-02 and ESC-03 from failed transactions into silent loss. **Fix:** `Precision::Exact` everywhere.

### ESC-11 High: a lock on the payer blocks every payout

Found in v1, after ESC-01 to ESC-10 were fixed. A lock (a conviction vote, say) applies to an account's whole balance, held funds included. v1's `release` moved the milestone out of the payer's hold with `Fortitude::Polite`, which `pallet-balances` limits to what the locks leave free: `reserved - (frozen - free)`. A payer who locks their balance makes every payout fail with `Token(Frozen)`, the arbiter's included, then refunds once the deadline passes. Refunds go through, since releasing a hold back to its owner does not reduce the balance a lock covers. The arbiter, the payer's counterweight in the threat model, can no longer pay the beneficiary.

The harness found it in 142 of 3,000 sequences and shrank it to five calls: two creates, a spend down to zero, a lock on the payer, and a release rejected with `Token(Frozen)`. It needed two changes to see it: a rejected call is a violation when the spec gives no reason for it (the old oracle only checked that a rejection changed nothing), and the harness now locks and spends from accounts between calls.

v0 is worse. Its `release` uses `Precision::BestEffort` (ESC-10), so under a lock it pays what it can, returns `Ok` and records the milestone as paid. In the shrunk reproduction, a lock and a release pay the beneficiary 694 of a 1,694 milestone.

**Fix in v1:** `Fortitude::Force` on the payout. The hold is already the beneficiary's claim; a lock the payer takes later must not outrank it. Test: `payer_lock_cannot_block_a_payout`.

**Fix in v2:** the funds leave the payer's account when the escrow is created, into an account of the escrow's own, so no lock on the payer reaches them. The same test runs against v2. The storage migration moves v1's holds with `Force` for the same reason.

## Checked and not an issue

- **Write-before-check ordering.** Several v0 paths write storage before a later fallible call. Since FRAME made every dispatchable transactional, an error rolls those writes back, so this is not a vulnerability on its own. The hardened pallet still orders checks first. `create_without_funds_changes_nothing` pins the rollback behaviour (first hold succeeds, second fails, nothing persists).
- **Block author manipulation of deadlines.** Deadlines are block numbers, which authors cannot move.

## Verification of the fixes

Measured on a development machine for this revision, with the commands CI runs:

| Check | Result |
|---|---|
| Unit tests, `pallets/escrow` (v2) | 31 pass, each followed by `try_state`, including the migration from the hold-based layout |
| Unit tests, `pallets/escrow-v1` | 21 pass, each followed by `try_state` |
| Benchmarks execute (`--features runtime-benchmarks`) | 8 pass; `frame-omni-bencher` runs all eight against `runtime/` |
| Spec-oracle harness, v2, 50,000 sequences | 0 violations: 72,944 creates, 22,493 releases, 3,623 refunds, 19,603 cancels, 8,106 submits, 1,511 disputes, 350 claims and 281 resolutions succeeded, 805,048 rejected calls each checked to carry a spec reason and change nothing |
| Spec-oracle harness, v1, 50,000 sequences | 0 violations on its part of the spec |
| Migration, 5,000 v1 chains | 3,833 live escrows migrated with nothing observable changed and no hold left, then held to the spec |
| Spec-oracle harness, v0, 20,000 sequences | ESC-01 to ESC-06, ESC-08 and ESC-11 rediscovered |
| `no_std` build for `wasm32v1-none` | v1 and v2 build |

Checked by CI only, not re-run for this revision:

| Check | Gate |
|---|---|
| libFuzzer, v2, 5 minutes per run | must not crash |
| libFuzzer, v0 | must crash (the `unwrap` in `release`) |
| frame-sentinel on v1 and v2 | no findings at medium or above |
