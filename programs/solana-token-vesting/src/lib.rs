use anchor_lang::prelude::*;
use anchor_spl::token::{close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer};

declare_id!("5h3bWCdzFrdPz2ZX5ECZEfAr9D97kdcieYgfteyjmS3q");

fn vested_amount(schedule: &VestingSchedule, now: i64) -> Result<u64> {
    let effective_time = if schedule.revoked {
        schedule.revoked_at
    } else {
        now
    };

    if effective_time < schedule.start_time + schedule.cliff_duration {
        return Ok(0);
    }
    if effective_time >= schedule.start_time + schedule.vesting_duration {
        return Ok(schedule.total_amount);
    }

    let elapsed = (effective_time - schedule.start_time) as u128;
    let vested = (schedule.total_amount as u128)
        .checked_mul(elapsed)
        .and_then(|v| v.checked_div(schedule.vesting_duration as u128))
        .ok_or(VestingError::MathOverflow)?;
    Ok(vested as u64)
}

#[program]
pub mod solana_token_vesting {
    use super::*;

    pub fn create_vesting(
        ctx: Context<CreateVesting>,
        vesting_id: u64,
        total_amount: u64,
        start_time: i64,
        cliff_duration: i64,
        vesting_duration: i64,
        revocable: bool,
    ) -> Result<()> {
        require!(total_amount > 0, VestingError::InvalidAmount);
        require!(
            vesting_duration > 0 && cliff_duration >= 0 && cliff_duration <= vesting_duration,
            VestingError::InvalidSchedule
        );

        transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.grantor_token_account.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.grantor.to_account_info(),
                },
            ),
            total_amount,
        )?;

        let schedule = &mut ctx.accounts.schedule;
        schedule.grantor = ctx.accounts.grantor.key();
        schedule.beneficiary = ctx.accounts.beneficiary.key();
        schedule.mint = ctx.accounts.mint.key();
        schedule.vesting_id = vesting_id;
        schedule.vault = ctx.accounts.vault.key();
        schedule.total_amount = total_amount;
        schedule.released_amount = 0;
        schedule.start_time = start_time;
        schedule.cliff_duration = cliff_duration;
        schedule.vesting_duration = vesting_duration;
        schedule.revocable = revocable;
        schedule.revoked = false;
        schedule.revoked_at = 0;
        schedule.bump = ctx.bumps.schedule;

        Ok(())
    }

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let schedule = &ctx.accounts.schedule;
        let vested = vested_amount(schedule, now)?;
        let claimable = vested
            .checked_sub(schedule.released_amount)
            .ok_or(VestingError::MathOverflow)?;
        require!(claimable > 0, VestingError::NothingToClaim);

        let grantor_key = schedule.grantor;
        let beneficiary_key = schedule.beneficiary;
        let mint_key = schedule.mint;
        let vesting_id_bytes = schedule.vesting_id.to_le_bytes();
        let bump = schedule.bump;
        let signer_seeds: &[&[u8]] = &[
            b"vesting",
            grantor_key.as_ref(),
            beneficiary_key.as_ref(),
            mint_key.as_ref(),
            vesting_id_bytes.as_ref(),
            &[bump],
        ];

        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.beneficiary_token_account.to_account_info(),
                    authority: ctx.accounts.schedule.to_account_info(),
                },
                &[signer_seeds],
            ),
            claimable,
        )?;

        ctx.accounts.schedule.released_amount = ctx
            .accounts
            .schedule
            .released_amount
            .checked_add(claimable)
            .ok_or(VestingError::MathOverflow)?;

        Ok(())
    }

    pub fn revoke(ctx: Context<Revoke>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        {
            let schedule = &ctx.accounts.schedule;
            require!(schedule.revocable, VestingError::NotRevocable);
            require!(!schedule.revoked, VestingError::AlreadyRevoked);
        }

        let vested = vested_amount(&ctx.accounts.schedule, now)?;
        let unvested = ctx
            .accounts
            .schedule
            .total_amount
            .checked_sub(vested)
            .ok_or(VestingError::MathOverflow)?;

        ctx.accounts.schedule.revoked = true;
        ctx.accounts.schedule.revoked_at = now;

        if unvested > 0 {
            let grantor_key = ctx.accounts.schedule.grantor;
            let beneficiary_key = ctx.accounts.schedule.beneficiary;
            let mint_key = ctx.accounts.schedule.mint;
            let vesting_id_bytes = ctx.accounts.schedule.vesting_id.to_le_bytes();
            let bump = ctx.accounts.schedule.bump;
            let signer_seeds: &[&[u8]] = &[
                b"vesting",
                grantor_key.as_ref(),
                beneficiary_key.as_ref(),
                mint_key.as_ref(),
                vesting_id_bytes.as_ref(),
                &[bump],
            ];

            transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.grantor_token_account.to_account_info(),
                        authority: ctx.accounts.schedule.to_account_info(),
                    },
                    &[signer_seeds],
                ),
                unvested,
            )?;
        }

        Ok(())
    }

    pub fn close_vesting(ctx: Context<CloseVesting>) -> Result<()> {
        require!(
            ctx.accounts.vault.amount == 0,
            VestingError::VaultNotEmpty
        );

        let grantor_key = ctx.accounts.schedule.grantor;
        let beneficiary_key = ctx.accounts.schedule.beneficiary;
        let mint_key = ctx.accounts.schedule.mint;
        let vesting_id_bytes = ctx.accounts.schedule.vesting_id.to_le_bytes();
        let bump = ctx.accounts.schedule.bump;
        let signer_seeds: &[&[u8]] = &[
            b"vesting",
            grantor_key.as_ref(),
            beneficiary_key.as_ref(),
            mint_key.as_ref(),
            vesting_id_bytes.as_ref(),
            &[bump],
        ];

        close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.grantor.to_account_info(),
                authority: ctx.accounts.schedule.to_account_info(),
            },
            &[signer_seeds],
        ))?;

        Ok(())
    }
}

#[account]
pub struct VestingSchedule {
    pub grantor: Pubkey,
    pub beneficiary: Pubkey,
    pub mint: Pubkey,
    pub vesting_id: u64,
    pub vault: Pubkey,
    pub total_amount: u64,
    pub released_amount: u64,
    pub start_time: i64,
    pub cliff_duration: i64,
    pub vesting_duration: i64,
    pub revocable: bool,
    pub revoked: bool,
    pub revoked_at: i64,
    pub bump: u8,
}

impl VestingSchedule {
    pub const MAX_SIZE: usize = 8 // discriminator
        + 32 * 4 // grantor, beneficiary, mint, vault
        + 8 // vesting_id
        + 8 // total_amount
        + 8 // released_amount
        + 8 // start_time
        + 8 // cliff_duration
        + 8 // vesting_duration
        + 1 // revocable
        + 1 // revoked
        + 8 // revoked_at
        + 1; // bump
}

#[derive(Accounts)]
#[instruction(vesting_id: u64)]
pub struct CreateVesting<'info> {
    #[account(mut)]
    pub grantor: Signer<'info>,

    /// CHECK: only used as a pubkey to derive/seed the schedule; never read or written directly.
    pub beneficiary: UncheckedAccount<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = grantor,
        space = VestingSchedule::MAX_SIZE,
        seeds = [b"vesting", grantor.key().as_ref(), beneficiary.key().as_ref(), mint.key().as_ref(), vesting_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub schedule: Account<'info, VestingSchedule>,

    #[account(
        init,
        payer = grantor,
        token::mint = mint,
        token::authority = schedule,
        seeds = [b"vault", schedule.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(mut)]
    pub grantor_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    pub beneficiary: Signer<'info>,

    #[account(
        mut,
        constraint = schedule.beneficiary == beneficiary.key() @ VestingError::Unauthorized,
        seeds = [b"vesting", schedule.grantor.as_ref(), schedule.beneficiary.as_ref(), schedule.mint.as_ref(), schedule.vesting_id.to_le_bytes().as_ref()],
        bump = schedule.bump,
    )]
    pub schedule: Account<'info, VestingSchedule>,

    #[account(mut)]
    pub beneficiary_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [b"vault", schedule.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Revoke<'info> {
    pub grantor: Signer<'info>,

    #[account(
        mut,
        constraint = schedule.grantor == grantor.key() @ VestingError::Unauthorized,
        seeds = [b"vesting", schedule.grantor.as_ref(), schedule.beneficiary.as_ref(), schedule.mint.as_ref(), schedule.vesting_id.to_le_bytes().as_ref()],
        bump = schedule.bump,
    )]
    pub schedule: Account<'info, VestingSchedule>,

    #[account(mut)]
    pub grantor_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [b"vault", schedule.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CloseVesting<'info> {
    #[account(mut)]
    pub grantor: Signer<'info>,

    #[account(
        mut,
        constraint = schedule.grantor == grantor.key() @ VestingError::Unauthorized,
        seeds = [b"vesting", schedule.grantor.as_ref(), schedule.beneficiary.as_ref(), schedule.mint.as_ref(), schedule.vesting_id.to_le_bytes().as_ref()],
        bump = schedule.bump,
        close = grantor,
    )]
    pub schedule: Account<'info, VestingSchedule>,

    #[account(
        mut,
        seeds = [b"vault", schedule.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[error_code]
pub enum VestingError {
    #[msg("Amount must be greater than zero")]
    InvalidAmount,
    #[msg("cliff_duration must be between 0 and vesting_duration")]
    InvalidSchedule,
    #[msg("This vesting schedule is not revocable")]
    NotRevocable,
    #[msg("This vesting schedule has already been revoked")]
    AlreadyRevoked,
    #[msg("Nothing is currently claimable")]
    NothingToClaim,
    #[msg("Vault still holds tokens; claim or revoke first")]
    VaultNotEmpty,
    #[msg("Signer is not authorized to perform this action")]
    Unauthorized,
    #[msg("Arithmetic overflow")]
    MathOverflow,
}
