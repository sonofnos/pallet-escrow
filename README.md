# pallet-escrow

A milestone escrow pallet for the Polkadot SDK (stable2606), written three times. First came a draft, which I reviewed myself as an auditor would. The hold-based pallet came out of that review, and the current pallet was redesigned after the harness found a flaw in the hold-based one (ESC-11). All three stay in the repository so every finding can be reproduced. This is a self-review, not an independent audit.

- **`pallets/escrow`**: the pallet. Each escrow's funds sit in an account of its own. The beneficiary can claim a milestone the payer does not dispute, and an arbiter settles disputes with a split. Every milestone has its own deadline. It has a storage version and a migration from the hold-based layout, and weights generated with `frame-omni-bencher`.
- **`pallets/escrow-v1`**: the hold-based pallet, where ESC-11 was found and fixed. Kept as a harness target and as the layout the migration starts from.
- **`pallets/escrow-v0`**: the first draft. Two critical, three high, two medium findings, plus its share of ESC-11. See [audit/REPORT.md](audit/REPORT.md). Never deploy it.
- **`harness/`**: a spec-oracle harness. It runs random call sequences against any of the three and checks every call against the written spec, then shrinks any failure to a minimal reproduction.
- **`fuzz/`**: the same harness under libFuzzer (`cargo fuzz`).
- **`runtime/`**: a minimal runtime the pallet is benchmarked in.
- **`audit/`**: [threat model](audit/THREAT_MODEL.md) and [review report](audit/REPORT.md).

## How the escrow works

A payer opens an escrow for a beneficiary with a list of milestones, each with a deadline, optionally naming an arbiter. The full amount plus a storage deposit moves into the escrow's own account. Milestones are paid in order:

| Call | Who | When |
|---|---|---|
| `release` | payer or arbiter | any time; pays the next milestone |
| `submit` | beneficiary | before the next milestone's deadline, if there is an arbiter |
| `dispute` | payer | within `ChallengePeriod` blocks of a submission |
| `claim` | beneficiary | once an undisputed submission is `ChallengePeriod` blocks old; pays the next milestone |
| `resolve` | arbiter | on a disputed submission; splits the milestone between the two sides |
| `refund` | arbiter | any time; everything left goes to the payer |
| `refund` | payer | once the next milestone's deadline has passed with nothing submitted |
| `cancel` | beneficiary | any time; everything left goes to the payer |

Refunds always go to the payer, whoever triggers them. The escrow closes when its last milestone settles or it is refunded or cancelled. Whatever is left in its account then goes to the payer, and the account is reaped.

The claim flow closes the gap the first two versions left open. There, a payer could wait out the deadline and refund work already delivered. Now a submitted milestone blocks the payer's refund, and it pays out unless the payer disputes it in time. Without an arbiter no one can settle a dispute, so the escrow trusts the payer, as before.

### The accounting invariant

For every live escrow, checked by `try_state`:

```text
balance(escrow account) >= remaining + deposit
remaining               == sum of unpaid milestones
```

An account per escrow is what makes this local. The first two versions held every escrow of a payer in one shared `pallet-balances` hold, so every path that moved money had to take exactly one escrow's share. Getting that wrong is what ESC-02 and ESC-03 are. Now the ledger itself keeps escrows apart, and a lock on the payer can no longer reach escrowed funds (ESC-11).

## The harness

The model does not re-implement the pallet. For each call it answers two questions: must this call fail, and for which reasons; and if it must not, exactly what does it do to every account's free balance, escrowed funds and deposits, to the stored escrows, and to the events. The driver holds the pallet to four rules:

1. A call the spec gives no reason to reject must succeed, with exactly the spec's effect on balances, storage and events.
2. A call the spec rejects must fail with one of the spec's reasons, and change nothing.
3. Nothing may panic, and total issuance never changes.
4. `try_state` holds after every call.

Rule 1 cuts both ways: a pallet that refuses a payout it owes fails as surely as one that pays the wrong account. Between calls the harness also acts as other pallets do on the same accounts. It locks balances, as a conviction vote does, and spends accounts down to the existential deposit or to nothing. Those two additions are how it found ESC-11.

One spec covers all three targets; the older two get the part of it they implement (no claims, one deadline per escrow). Callers are chosen by role and escrow ids favour escrows where the call can matter, so sequences reach claims, disputes and resolutions instead of stopping at input validation. Call counters are printed with every run, so a clean result can be told apart from one that never got anywhere.

The migration is checked the same way, on state the hold-based pallet actually produced. Random sequences run against v1, the storage moves into a runtime with the current pallet, and the migration runs. Nothing observable may change and no hold may remain. Then the migrated escrows must follow the spec.

Measured on a development machine, with the same commands CI runs:

| Target | Sequences | Result |
|---|---|---|
| `pallets/escrow` | 50,000 | **0 violations** across 72,944 creates, 22,493 releases, 3,623 refunds, 19,603 cancels, 8,106 submits, 1,511 disputes, 350 claims and 281 resolutions, plus 805,048 rejected calls |
| `pallets/escrow-v1` | 50,000 | **0 violations** on its part of the spec |
| `pallets/escrow-v1` → `pallets/escrow` | 5,000 | 3,833 live escrows migrated with nothing observable changed, then held to the spec |
| `pallets/escrow-v0` | 20,000 | 15 violation classes, covering every finding in the report, each shrunk to a 1 to 3 call reproduction |

These results are evidence, not proof. They show the pallet survives this spec, this corpus and this fuzzing configuration, while the earlier versions fail it in the ways the report describes.

```
cargo test -p escrow-harness --release                              # CI gate: v2 and v1 pass, v0 findings rediscovered, migration keeps state
cargo run --release -p escrow-harness --example hunt -- v0 20000    # every violation class with a shrunk repro (or v1, v2)
cargo run --release -p escrow-harness --example poc                 # the three fund-loss exploits, with balances
cd fuzz && cargo +nightly fuzz run escrow                           # coverage-guided, the pallet
```

## Running

```
cargo test -p pallet-escrow                                          # unit tests, try_state after each
cargo test -p pallet-escrow --features runtime-benchmarks,try-runtime  # benchmarks execute, migration hooks build
cargo check -p pallet-escrow --no-default-features --target wasm32v1-none
```

Weights come from `frame-omni-bencher` run against `runtime/`:

```
cargo build --release -p escrow-bench-runtime --features runtime-benchmarks
frame-omni-bencher v1 benchmark pallet \
  --runtime target/release/wbuild/escrow-bench-runtime/escrow_bench_runtime.compact.compressed.wasm \
  --pallet pallet_escrow --extrinsic '*' --steps 50 --repeat 20 \
  --template .maintain/frame-weight-template.hbs --header .maintain/weights-header.txt \
  --output pallets/escrow/src/weights.rs
```

CI runs all of the above on every push, plus clippy (`-D warnings`), rustfmt, the fuzz targets, the release-build proof of concept and frame-sentinel.

## Known limits

- The weights were measured on a development machine, not reference hardware; `weights.rs` says which. They replace hand-set guesses that under-declared proof size by about a third. Regenerate on reference hardware before production use.
- The harness uses five accounts. It explores sequences, not large populations. No call iterates storage, and storage per escrow is bounded and paid for, so the number of escrows does not change what a call costs.
- The arbiter is trusted to decide fairly; the pallet only guarantees that an arbiter can never receive the funds. An arbiter that never acts leaves a disputed milestone waiting until the payer releases or the beneficiary cancels.
- The migration runs in a single block.
