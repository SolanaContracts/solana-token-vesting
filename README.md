# solana-token-vesting

An Anchor program for token vesting / lockup: a grantor locks up SPL tokens for a beneficiary, which release gradually over time — the same pattern every project with a token uses for investors, team, and treasury grants.

## Why this shape

This implements the classic **cliff + linear vesting schedule** (the same shape as OpenZeppelin's `VestingWallet` and most token-vesting contracts): a grant vests linearly from `start_time` over `vesting_duration`, but nothing is claimable until `cliff_duration` has passed. Crossing the cliff releases everything accrued since the start in one lump sum, then continues releasing linearly after — e.g. a 4-year vest with a 1-year cliff pays out 25% the moment the cliff passes, not 0%.

It also implements **revocation that preserves already-earned rights**: a revoked grant freezes the vested amount at the moment of revocation. The beneficiary keeps exactly what they'd already earned — no more, no less — and only the *unvested* remainder returns to the grantor. Getting this subtly wrong (e.g. letting revocation claw back already-vested tokens, or letting vesting continue after revocation) is a real, easy-to-make bug in naive implementations.

## Instructions

| Instruction | Signer | Description |
|---|---|---|
| `create_vesting(vesting_id, total_amount, start_time, cliff_duration, vesting_duration, revocable)` | grantor | Creates a `VestingSchedule` + vault and deposits `total_amount` tokens. |
| `claim()` | beneficiary | Releases whatever has vested and hasn't been claimed yet. |
| `revoke()` | grantor | Only if `revocable`. Freezes the vested amount at "now", refunds the unvested remainder to the grantor immediately. |
| `close_vesting()` | grantor | Once the vault is empty (fully claimed, or revoked + fully settled), closes the vault + schedule and reclaims rent. |

## Accounts

**`VestingSchedule`** — PDA at `["vesting", grantor, beneficiary, mint, vesting_id]`
- `grantor`, `beneficiary`, `mint`, `vesting_id`, `vault`
- `total_amount`, `released_amount`
- `start_time`, `cliff_duration`, `vesting_duration` (seconds)
- `revocable`, `revoked`, `revoked_at`

## The vesting formula

```rust
fn vested_amount(schedule, now) -> u64 {
    let effective_time = if schedule.revoked { schedule.revoked_at } else { now };
    if effective_time < schedule.start_time + schedule.cliff_duration {
        0
    } else if effective_time >= schedule.start_time + schedule.vesting_duration {
        schedule.total_amount
    } else {
        schedule.total_amount * (effective_time - schedule.start_time) / schedule.vesting_duration
    }
}
```

Once revoked, `effective_time` is pinned to `revoked_at` forever — this is what stops the beneficiary from earning anything past the moment of revocation while still honoring what they'd already earned up to that point.

## Building and testing

Requires `solana-cli`, `anchor-cli`, and Rust already installed. This machine needed `platform-tools` v1.57 to avoid an `edition2024` build error (same issue as the other repos in this org).

```bash
anchor build --no-idl -- --tools-version v1.57
anchor idl build -o target/idl/solana_token_vesting.json -t target/types/solana_token_vesting.ts
anchor test --skip-build --no-idl
```

`cargo clippy` (run from `programs/solana-token-vesting`) is clean.

The test suite's revoke check doesn't eyeball the numbers — it reads the actual on-chain `revoked_at` timestamp after revoking, recomputes the expected vested amount in TypeScript using the exact same formula, and asserts the grantor's refund and the beneficiary's final claim both match that computed value exactly.

## CLI client

`cli/` is a Rust CLI (`vesting-cli`, built with `anchor-client` + `clap`) covering every instruction, plus a `show` command that previews the currently vested/claimable amount using the host's clock. Defaults to a local validator (`http://127.0.0.1:8899` / `ws://127.0.0.1:8900`) — override with `--url`/`--ws-url` for devnet or mainnet.

Note: `show`'s preview uses your local machine's clock, while the program itself always uses the validator's on-chain clock (`Clock::get()`) at the moment a transaction actually lands — these can drift apart by a few seconds, especially against a freshly-reset local test validator still catching up. The on-chain value is always the source of truth.

```bash
cargo build -p vesting-cli
BIN=./target/debug/vesting-cli

# local validator + program deploy + a test mint:
solana-test-validator --reset --quiet &
solana program deploy target/deploy/solana_token_vesting.so \
  --program-id target/deploy/solana_token_vesting-keypair.json

$BIN create-vesting --keypair ~/grantor.json --beneficiary <BENEFICIARY_PUBKEY> \
  --mint <MINT> --vesting-id 1 --total-amount 1000000 \
  --cliff-seconds 31536000 --vesting-seconds 126144000 --revocable
  # (1-year cliff, 4-year vest, in seconds; omit --start-unix to start now)

$BIN show --grantor <GRANTOR_PUBKEY> --beneficiary <BENEFICIARY_PUBKEY> --mint <MINT> --vesting-id 1

$BIN claim --keypair ~/beneficiary.json --grantor <GRANTOR_PUBKEY> \
  --beneficiary <BENEFICIARY_PUBKEY> --mint <MINT> --vesting-id 1

$BIN revoke --keypair ~/grantor.json --beneficiary <BENEFICIARY_PUBKEY> --mint <MINT> --vesting-id 1

$BIN close-vesting --keypair ~/grantor.json --beneficiary <BENEFICIARY_PUBKEY> --mint <MINT> --vesting-id 1
```

Run `$BIN --help` or `$BIN <command> --help` for the full flag list.
