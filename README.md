# pallet-escrow

A milestone escrow pallet for the Polkadot SDK (stable2606), written twice: a first draft that I reviewed as an auditor would, and the hardened pallet that came out of that review. Both stay in the repository so every finding can be reproduced.

- **`pallets/escrow`**: the pallet. Funds are held with `fungible::MutateHold`, storage is bounded, every call has a benchmark, and `try_state` checks that hold balances equal the sum over live escrows.
- **`pallets/escrow-v0`**: the audit target. Two critical, three high, two medium findings. See [audit/REPORT.md](audit/REPORT.md). Never deploy it.
- **`harness/`**: a spec-oracle harness. It runs random call sequences against either pallet and checks every call against the written spec, then shrinks any failure to a minimal reproduction.
- **`fuzz/`**: the same harness under libFuzzer (`cargo fuzz`).
- **`audit/`**: [threat model](audit/THREAT_MODEL.md) and [review report](audit/REPORT.md).

## How the escrow works

A payer opens an escrow for a beneficiary with a list of milestones and a deadline, optionally naming an arbiter. The full amount plus a storage deposit is held from the payer's balance. The payer or arbiter releases milestones in order. The payer can reclaim the rest after the deadline; the arbiter can refund at any time; the beneficiary can cancel. Refunds always go to the payer, whoever triggers them.

The subtle part: `pallet-balances` holds are per account and reason, not per escrow, so all of a payer's escrows share one `Escrow` hold. Every path that moves money must move exactly one escrow's share and close that escrow in the same transaction. Two of v0's worst bugs come from getting this wrong.

## The harness

The model does not re-implement the pallet. For each call it answers two questions: is this call allowed by the spec, and if it succeeds, exactly how much must each account's free balance and each hold move? The driver then holds the pallet to four rules:

1. A call that returns `Ok` must be allowed and must have exactly the spec's effect on every account.
2. A call that returns `Err` must change nothing.
3. Nothing may panic.
4. Total issuance never changes, and `try_state` holds after every call.

Most generated creates are well formed and callers are chosen by role (payer, beneficiary, arbiter, stranger), so sequences reach deep states rather than stopping at input validation. Call counters are printed with every run so a clean result can be told apart from one that never got anywhere.

Results from CI:

| Target | Sequences | Result |
|---|---|---|
| hardened pallet | 50,000 | **0 violations** across 221,365 successful creates, 50,026 releases, 33,487 refunds, 22,893 cancels and 792,547 rejected calls |
| v0 | 20,000 | 15 violation classes, covering every finding in the report, each shrunk to a 1 to 3 call reproduction |

```
cargo test -p escrow-harness --release                              # CI gate: fixed passes, v0 findings rediscovered
cargo run --release -p escrow-harness --example hunt -- v0 20000    # every violation class with a shrunk repro
cargo run --release -p escrow-harness --example poc                 # the three fund-loss exploits, with balances
cd fuzz && cargo +nightly fuzz run escrow                           # coverage-guided, hardened pallet
```

## Running

```
cargo test -p pallet-escrow                                   # unit tests, try_state after each
cargo test -p pallet-escrow --features runtime-benchmarks     # benchmarks execute
cargo check -p pallet-escrow --no-default-features --target wasm32v1-none
```

CI runs all of the above plus clippy (`-D warnings`), rustfmt, the fuzz targets and the release-build proof of concept on every push.

## Known limits

- Weights are hand-set upper bounds. The benchmarks exist and run in CI, but have not been run on reference hardware to generate `weights.rs`.
- The harness uses five accounts and `u64` balances. It explores sequences, not large populations.
- The arbiter is trusted to be fair; the pallet only guarantees that an arbiter can never receive the funds.
