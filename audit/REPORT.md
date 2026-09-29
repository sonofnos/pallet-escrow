# Security review: pallet-escrow-v0

| | |
|---|---|
| Target | `pallets/escrow-v0` (the first draft, kept unchanged as the audit target) |
| Framework | Polkadot SDK stable2606 (`frame-support` 48, `pallet-balances` 50) |
| Methods | Manual review against the [threat model](THREAT_MODEL.md); static analysis with [frame-sentinel](https://github.com/sonofnos/frame-sentinel); spec-oracle harness driven by seeded sequences and libFuzzer (`harness/`, `fuzz/`) |
| Outcome | 10 findings: 2 critical, 3 high, 2 medium, 1 low, 2 informational. All fixed in `pallets/escrow`, each with a regression test |

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

Severity follows impact and likelihood. **Critical** means direct loss of user funds by an unprivileged or semi-trusted party. **High** means loss of funds needing collusion, or a broken core guarantee. **Medium** means liveness or cost problems. **Low** means incorrect behaviour without direct loss.

The harness column is not a claim made after the fact. `harness/tests/invariants.rs` runs 20,000 seeded sequences against v0 in CI and fails unless it rediscovers ESC-01 to ESC-06 on its own, and `harness/examples/poc.rs` reproduces the three fund-loss findings with balances printed. The numbers below are from those runs.

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

The harness reaches this through three different symptoms (a second `cancel`, a `release`, or a `refund` succeeding on an escrow that should be closed), found in 117, 177 and 250 of 20,000 sequences.

**Fix:** one `close()` path releases this escrow's own funds with `Precision::Exact` and deletes it in the same transaction. Test: `cancel_is_final_and_leaves_other_escrows_funded`.

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

In a debug build the same input panics (the harness reports `create: arithmetic overflow panic`). Run as a runtime runs, it reports `create: succeeded but milestone total overflows`. Both CI jobs are kept (`hunt-report`).

**Fix:** `checked_add` with `Error::Overflow`. Test: `milestone_overflow_is_rejected`.

### ESC-04 High: any account can release

`release` calls `ensure_signed(origin)?` and discards the account. Anyone can push every milestone to the beneficiary immediately, removing the payer's control over timing, which is the only reason the escrow exists. Harness: `release: succeeded but caller may not release`, 1,605 of 20,000 sequences.

**Fix:** payer or arbiter only. Test: `stranger_and_beneficiary_cannot_release`.

### ESC-05 High: payer can refund before the deadline

`refund` accepts the payer at any time. A beneficiary who delivers work has no guarantee: the payer can watch for delivery and pull the funds first. Harness: `refund: succeeded but payer refunded before the deadline`, 843 of 20,000 sequences.

**Fix:** the payer may refund only from `deadline`; the arbiter at any time. Test: `payer_refund_waits_for_deadline`.

### ESC-06 Medium: panics in dispatch

`Escrows::<T>::get(id).unwrap()` panics on an unknown id. Because a completed escrow is never deleted, `escrow.milestones[escrow.released as usize]` panics on the next `release`. A panicking extrinsic is dropped from the block without paying its fee, so anyone can make block authors do work for free. Harness: `release: unwrap on missing escrow panic` (first seed 0) and `release: index out of bounds panic` (292 sequences). frame-sentinel: FS001 (high), FS008.

**Fix:** `ok_or(Error::NotFound)`, `.get()` with `Error::Inconsistent`, delete on completion. Test: `unknown_or_finished_escrow_is_an_error_not_a_panic`.

### ESC-07 Medium: zero weight, unbounded storage, no deposit

Every call is `#[pallet::weight(Weight::zero())]`, the pallet is `#[pallet::without_storage_info]`, milestones are an unbounded `Vec`, and creating an escrow costs no deposit. Calls are free block space and each escrow is permanent state at no cost. frame-sentinel: FS006 ×4, FS004.

**Fix:** `BoundedVec<_, MaxMilestones>`, `MaxEncodedLen` storage, an `EscrowDeposit` hold returned on close, and weights from `WeightInfo` with benchmarks covering every call (`benchmarking.rs`; the weights themselves are hand-set upper bounds until run on reference hardware).

### ESC-08 Low: no input validation on `create`

Empty milestone lists, zero-value milestones, `payer == beneficiary`, an arbiter who is a party, and deadlines in the past are all accepted. None moves funds wrongly on its own, but each creates escrows whose guarantees are meaningless. The past-deadline case turns ESC-05 into an immediate refund even after the fix. Harness: each of the five, 1 to 1,302 sequences.

**Fix:** `NoMilestones`, `MilestoneTooSmall` (with `MinMilestone >= ED` checked in `integrity_test`), `SelfEscrow`, `ArbiterConflict`, `DeadlineInPast`. Test: `create_rejects_bad_input`.

### ESC-09 Info: unchecked id increment

`NextEscrowId::<T>::put(id + 1)` wraps at `u32::MAX` and would overwrite escrow 0. Unreachable in practice once deposits exist, but free to fix. **Fix:** `checked_add` with `IdsExhausted`.

### ESC-10 Info: `BestEffort` hides accounting errors

Every transfer and release uses `Precision::BestEffort`, so an attempt to move more than is held silently moves less instead of failing. That is what turns ESC-02 and ESC-03 from failed transactions into silent loss. **Fix:** `Precision::Exact` everywhere.

## Checked and not an issue

- **Write-before-check ordering.** Several v0 paths write storage before a later fallible call. Since FRAME made every dispatchable transactional, an error rolls those writes back, so this is not a vulnerability on its own. The hardened pallet still orders checks first. `create_without_funds_changes_nothing` pins the rollback behaviour (first hold succeeds, second fails, nothing persists).
- **Block author manipulation of deadlines.** Deadlines are block numbers, which authors cannot move.

## Verification of the fixes

| Check | Result |
|---|---|
| Unit tests (`pallets/escrow`) | 18 pass, each followed by `try_state` |
| Benchmarks execute (`--features runtime-benchmarks`) | 4 pass |
| Spec-oracle harness, hardened pallet, 50,000 sequences | 0 violations: 221,365 successful creates, 50,026 releases, 33,487 refunds, 22,893 cancels (plus 792,547 rejected calls, each checked to change nothing) |
| libFuzzer, hardened pallet, 5 minutes per CI run | no crash in 114,367 executions (4,101 coverage points) |
| libFuzzer, v0 | crashes within about 1,300 executions (the `unwrap` in `release`); CI fails if it does not |
| frame-sentinel on the hardened pallet | 0 findings |
| `no_std` build for `wasm32v1-none` | builds |
