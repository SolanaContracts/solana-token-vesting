use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use anchor_client::{
    solana_sdk::{
        commitment_config::CommitmentConfig,
        pubkey::Pubkey,
        signature::{read_keypair_file, Keypair, Signer},
    },
    Client, Cluster,
};
use anchor_spl::associated_token::get_associated_token_address;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use solana_token_vesting::{accounts, instruction, VestingSchedule};

#[derive(Parser)]
#[command(name = "vesting-cli", about = "CLI client for the solana-token-vesting Anchor program")]
struct Cli {
    /// JSON-RPC URL of the cluster to talk to
    #[arg(long, global = true, default_value = "http://127.0.0.1:8899")]
    url: String,

    /// WebSocket URL of the cluster (used for transaction confirmation)
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8900")]
    ws_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new vesting grant
    CreateVesting {
        /// Keypair file for the grantor (pays for setup and supplies the tokens)
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        beneficiary: Pubkey,
        #[arg(long)]
        mint: Pubkey,
        /// Arbitrary id so one grantor can create multiple grants (even to the same beneficiary/mint)
        #[arg(long)]
        vesting_id: u64,
        /// Total tokens to vest, in the mint's base units
        #[arg(long)]
        total_amount: u64,
        /// Unix timestamp the vesting clock starts at; defaults to now
        #[arg(long)]
        start_unix: Option<i64>,
        /// Seconds after start_unix before anything is claimable
        #[arg(long)]
        cliff_seconds: i64,
        /// Total vesting length in seconds, measured from start_unix
        #[arg(long)]
        vesting_seconds: i64,
        /// Whether the grantor can revoke this grant later
        #[arg(long)]
        revocable: bool,
    },
    /// Claim whatever has vested and hasn't been claimed yet
    Claim {
        /// Keypair file for the beneficiary
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        grantor: Pubkey,
        #[arg(long)]
        beneficiary: Pubkey,
        #[arg(long)]
        mint: Pubkey,
        #[arg(long)]
        vesting_id: u64,
    },
    /// Revoke a revocable grant (grantor only)
    Revoke {
        /// Keypair file for the grantor
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        beneficiary: Pubkey,
        #[arg(long)]
        mint: Pubkey,
        #[arg(long)]
        vesting_id: u64,
    },
    /// Close a fully-settled grant and reclaim rent (grantor only)
    CloseVesting {
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        beneficiary: Pubkey,
        #[arg(long)]
        mint: Pubkey,
        #[arg(long)]
        vesting_id: u64,
    },
    /// Print a grant's current on-chain state
    Show {
        #[arg(long)]
        grantor: Pubkey,
        #[arg(long)]
        beneficiary: Pubkey,
        #[arg(long)]
        mint: Pubkey,
        #[arg(long)]
        vesting_id: u64,
    },
}

fn schedule_pda(grantor: &Pubkey, beneficiary: &Pubkey, mint: &Pubkey, vesting_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"vesting",
            grantor.as_ref(),
            beneficiary.as_ref(),
            mint.as_ref(),
            &vesting_id.to_le_bytes(),
        ],
        &solana_token_vesting::ID,
    )
    .0
}

fn vault_pda(schedule: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", schedule.as_ref()], &solana_token_vesting::ID).0
}

fn load_keypair(path: &PathBuf) -> Result<Keypair> {
    read_keypair_file(path)
        .map_err(|e| anyhow::anyhow!("failed to read keypair at {}: {e}", path.display()))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before unix epoch")
        .as_secs() as i64
}

fn vested_amount(schedule: &VestingSchedule, now: i64) -> u64 {
    let effective_time = if schedule.revoked { schedule.revoked_at } else { now };
    if effective_time < schedule.start_time + schedule.cliff_duration {
        0
    } else if effective_time >= schedule.start_time + schedule.vesting_duration {
        schedule.total_amount
    } else {
        let elapsed = (effective_time - schedule.start_time) as u128;
        ((schedule.total_amount as u128 * elapsed) / schedule.vesting_duration as u128) as u64
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cluster = Cluster::Custom(cli.url.clone(), cli.ws_url.clone());

    match cli.command {
        Command::CreateVesting {
            keypair,
            beneficiary,
            mint,
            vesting_id,
            total_amount,
            start_unix,
            cliff_seconds,
            vesting_seconds,
            revocable,
        } => {
            let grantor = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, grantor.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_token_vesting::ID)?;
            let schedule = schedule_pda(&grantor.pubkey(), &beneficiary, &mint, vesting_id);
            let vault = vault_pda(&schedule);
            let grantor_token_account = get_associated_token_address(&grantor.pubkey(), &mint);
            let start_time = start_unix.unwrap_or_else(now_unix);

            let sig = program
                .request()
                .accounts(accounts::CreateVesting {
                    grantor: grantor.pubkey(),
                    beneficiary,
                    mint,
                    schedule,
                    vault,
                    grantor_token_account,
                    token_program: anchor_spl::token::ID,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                    rent: anchor_client::solana_sdk::sysvar::rent::ID,
                })
                .args(instruction::CreateVesting {
                    vesting_id,
                    total_amount,
                    start_time,
                    cliff_duration: cliff_seconds,
                    vesting_duration: vesting_seconds,
                    revocable,
                })
                .send()
                .context("create_vesting transaction failed")?;

            println!("Vesting schedule created at {schedule}");
            println!("Vault: {vault}");
            println!("Signature: {sig}");
        }

        Command::Claim {
            keypair,
            grantor,
            beneficiary,
            mint,
            vesting_id,
        } => {
            let beneficiary_kp = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, beneficiary_kp.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_token_vesting::ID)?;
            let schedule = schedule_pda(&grantor, &beneficiary, &mint, vesting_id);
            let vault = vault_pda(&schedule);
            let beneficiary_token_account = get_associated_token_address(&beneficiary_kp.pubkey(), &mint);

            let sig = program
                .request()
                .accounts(accounts::Claim {
                    beneficiary: beneficiary_kp.pubkey(),
                    schedule,
                    beneficiary_token_account,
                    vault,
                    token_program: anchor_spl::token::ID,
                })
                .args(instruction::Claim {})
                .send()
                .context("claim transaction failed")?;

            println!("Claimed vested tokens from {schedule}");
            println!("Signature: {sig}");
        }

        Command::Revoke {
            keypair,
            beneficiary,
            mint,
            vesting_id,
        } => {
            let grantor = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, grantor.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_token_vesting::ID)?;
            let schedule = schedule_pda(&grantor.pubkey(), &beneficiary, &mint, vesting_id);
            let vault = vault_pda(&schedule);
            let grantor_token_account = get_associated_token_address(&grantor.pubkey(), &mint);

            let sig = program
                .request()
                .accounts(accounts::Revoke {
                    grantor: grantor.pubkey(),
                    schedule,
                    grantor_token_account,
                    vault,
                    token_program: anchor_spl::token::ID,
                })
                .args(instruction::Revoke {})
                .send()
                .context("revoke transaction failed")?;

            println!("Revoked vesting schedule {schedule}");
            println!("Signature: {sig}");
        }

        Command::CloseVesting {
            keypair,
            beneficiary,
            mint,
            vesting_id,
        } => {
            let grantor = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, grantor.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_token_vesting::ID)?;
            let schedule = schedule_pda(&grantor.pubkey(), &beneficiary, &mint, vesting_id);
            let vault = vault_pda(&schedule);

            let sig = program
                .request()
                .accounts(accounts::CloseVesting {
                    grantor: grantor.pubkey(),
                    schedule,
                    vault,
                    token_program: anchor_spl::token::ID,
                })
                .args(instruction::CloseVesting {})
                .send()
                .context("close_vesting transaction failed")?;

            println!("Closed vesting schedule {schedule}");
            println!("Signature: {sig}");
        }

        Command::Show {
            grantor,
            beneficiary,
            mint,
            vesting_id,
        } => {
            let dummy_payer = Rc::new(Keypair::new());
            let client = Client::new_with_options(cluster, dummy_payer, CommitmentConfig::confirmed());
            let program = client.program(solana_token_vesting::ID)?;
            let schedule_key = schedule_pda(&grantor, &beneficiary, &mint, vesting_id);
            let schedule: VestingSchedule = program
                .account(schedule_key)
                .context("failed to fetch vesting schedule (does it exist?)")?;

            let now = now_unix();
            let vested = vested_amount(&schedule, now);
            let claimable = vested.saturating_sub(schedule.released_amount);

            println!("VestingSchedule: {schedule_key}");
            println!("  grantor:            {}", schedule.grantor);
            println!("  beneficiary:        {}", schedule.beneficiary);
            println!("  mint:               {}", schedule.mint);
            println!("  total_amount:       {}", schedule.total_amount);
            println!("  released_amount:    {}", schedule.released_amount);
            println!("  start_time:         {}", schedule.start_time);
            println!("  cliff_duration:     {} sec", schedule.cliff_duration);
            println!("  vesting_duration:   {} sec", schedule.vesting_duration);
            println!("  revocable:          {}", schedule.revocable);
            println!("  revoked:            {}", schedule.revoked);
            if schedule.revoked {
                println!("  revoked_at:         {}", schedule.revoked_at);
            }
            println!("  vested as of now:   {vested}");
            println!("  currently claimable:{claimable}");
        }
    }

    Ok(())
}
