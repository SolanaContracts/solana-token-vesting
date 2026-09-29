import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SystemProgram, LAMPORTS_PER_SOL } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { assert } from "chai";
import { SolanaTokenVesting } from "../target/types/solana_token_vesting";

describe("solana-token-vesting", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.getProvider() as anchor.AnchorProvider;
  const program = anchor.workspace
    .solanaTokenVesting as Program<SolanaTokenVesting>;

  const grantor = Keypair.generate();
  const beneficiary = Keypair.generate();
  const outsider = Keypair.generate();

  let mint: PublicKey;
  let grantorTokenAccount: PublicKey;
  let beneficiaryTokenAccount: PublicKey;

  const TOTAL_AMOUNT = 120;
  const CLIFF_SECONDS = 4;
  const VESTING_SECONDS = 12;

  const findSchedulePda = (vestingId: BN) =>
    PublicKey.findProgramAddressSync(
      [
        Buffer.from("vesting"),
        grantor.publicKey.toBuffer(),
        beneficiary.publicKey.toBuffer(),
        mint.toBuffer(),
        vestingId.toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    )[0];

  const findVaultPda = (schedule: PublicKey) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), schedule.toBuffer()],
      program.programId
    )[0];

  before(async () => {
    for (const kp of [grantor, beneficiary, outsider]) {
      const sig = await provider.connection.requestAirdrop(kp.publicKey, 2 * LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction(sig, "confirmed");
    }

    mint = await createMint(provider.connection, grantor, grantor.publicKey, null, 0);
    grantorTokenAccount = await createAccount(provider.connection, grantor, mint, grantor.publicKey);
    beneficiaryTokenAccount = await createAccount(provider.connection, beneficiary, mint, beneficiary.publicKey);

    await mintTo(provider.connection, grantor, mint, grantorTokenAccount, grantor, TOTAL_AMOUNT * 10);
  });

  async function createSchedule(vestingId: number, revocable: boolean) {
    const id = new BN(vestingId);
    const schedule = findSchedulePda(id);
    const vault = findVaultPda(schedule);
    const nowSec = Math.floor(Date.now() / 1000);

    await program.methods
      .createVesting(
        id,
        new BN(TOTAL_AMOUNT),
        new BN(nowSec),
        new BN(CLIFF_SECONDS),
        new BN(VESTING_SECONDS),
        revocable
      )
      .accounts({
        grantor: grantor.publicKey,
        beneficiary: beneficiary.publicKey,
        mint,
        schedule,
        vault,
        grantorTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .signers([grantor])
      .rpc();

    return { id, schedule, vault };
  }

  function expectedVested(schedule: any, atUnixSec: number): number {
    const start = schedule.startTime.toNumber();
    const cliff = schedule.cliffDuration.toNumber();
    const duration = schedule.vestingDuration.toNumber();
    const total = schedule.totalAmount.toNumber();
    const effective = schedule.revoked ? schedule.revokedAt.toNumber() : atUnixSec;
    if (effective < start + cliff) return 0;
    if (effective >= start + duration) return total;
    return Math.floor((total * (effective - start)) / duration);
  }

  it("rejects claiming before the cliff", async () => {
    const { schedule, vault } = await createSchedule(1, true);

    try {
      await program.methods
        .claim()
        .accounts({
          beneficiary: beneficiary.publicKey,
          schedule,
          beneficiaryTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([beneficiary])
        .rpc();
      assert.fail("expected claim to fail before the cliff");
    } catch (err) {
      assert.include(String(err), "NothingToClaim");
    }
  });

  it("rejects a non-beneficiary from claiming", async () => {
    const schedule = findSchedulePda(new BN(1));
    const vault = findVaultPda(schedule);

    try {
      await program.methods
        .claim()
        .accounts({
          beneficiary: outsider.publicKey,
          schedule,
          beneficiaryTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([outsider])
        .rpc();
      assert.fail("expected claim to fail for a non-beneficiary");
    } catch (err) {
      assert.include(String(err), "Unauthorized");
    }
  });

  it("releases a lump sum once the cliff passes, then fully vests by the end", async () => {
    const schedule = findSchedulePda(new BN(1));
    const vault = findVaultPda(schedule);

    await new Promise((resolve) => setTimeout(resolve, (CLIFF_SECONDS + 1) * 1000));

    await program.methods
      .claim()
      .accounts({
        beneficiary: beneficiary.publicKey,
        schedule,
        beneficiaryTokenAccount,
        vault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([beneficiary])
      .rpc();

    const afterCliff = (await getAccount(provider.connection, beneficiaryTokenAccount)).amount;
    assert.isTrue(afterCliff > 0n, "should have received the lump sum accrued since start");
    assert.isTrue(afterCliff < BigInt(TOTAL_AMOUNT), "should not be fully vested yet");

    await new Promise((resolve) => setTimeout(resolve, (VESTING_SECONDS - CLIFF_SECONDS + 1) * 1000));

    await program.methods
      .claim()
      .accounts({
        beneficiary: beneficiary.publicKey,
        schedule,
        beneficiaryTokenAccount,
        vault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([beneficiary])
      .rpc();

    const afterFull = (await getAccount(provider.connection, beneficiaryTokenAccount)).amount;
    assert.equal(afterFull.toString(), TOTAL_AMOUNT.toString());

    const scheduleAccount = await program.account.vestingSchedule.fetch(schedule);
    assert.equal(scheduleAccount.releasedAmount.toNumber(), TOTAL_AMOUNT);
  });

  it("closes a fully-claimed schedule and reclaims rent", async () => {
    const schedule = findSchedulePda(new BN(1));
    const vault = findVaultPda(schedule);

    await program.methods
      .closeVesting()
      .accounts({
        grantor: grantor.publicKey,
        schedule,
        vault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([grantor])
      .rpc();

    const closed = await program.account.vestingSchedule.fetchNullable(schedule);
    assert.isNull(closed);
  });

  it("rejects revoking a non-revocable schedule", async () => {
    const { schedule, vault } = await createSchedule(2, false);

    try {
      await program.methods
        .revoke()
        .accounts({
          grantor: grantor.publicKey,
          schedule,
          grantorTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([grantor])
        .rpc();
      assert.fail("expected revoke to fail on a non-revocable schedule");
    } catch (err) {
      assert.include(String(err), "NotRevocable");
    }
  });

  it("freezes the vested amount on revoke, refunds the rest, and blocks further accrual", async () => {
    const { schedule, vault } = await createSchedule(3, true);

    // wait past the cliff but well before full vesting
    await new Promise((resolve) => setTimeout(resolve, (CLIFF_SECONDS + 2) * 1000));

    const grantorBalanceBefore = (await getAccount(provider.connection, grantorTokenAccount)).amount;

    await program.methods
      .revoke()
      .accounts({
        grantor: grantor.publicKey,
        schedule,
        grantorTokenAccount,
        vault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([grantor])
      .rpc();

    const scheduleAccount = await program.account.vestingSchedule.fetch(schedule);
    assert.isTrue(scheduleAccount.revoked);

    const expectedVestedAtRevoke = expectedVested(scheduleAccount, scheduleAccount.revokedAt.toNumber());
    const expectedRefund = TOTAL_AMOUNT - expectedVestedAtRevoke;

    const grantorBalanceAfter = (await getAccount(provider.connection, grantorTokenAccount)).amount;
    assert.equal(
      (grantorBalanceAfter - grantorBalanceBefore).toString(),
      expectedRefund.toString()
    );

    // beneficiary claims exactly what had vested at revocation time
    const beneficiaryBalanceBefore = (await getAccount(provider.connection, beneficiaryTokenAccount)).amount;
    await program.methods
      .claim()
      .accounts({
        beneficiary: beneficiary.publicKey,
        schedule,
        beneficiaryTokenAccount,
        vault,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([beneficiary])
      .rpc();
    const beneficiaryBalanceAfter = (await getAccount(provider.connection, beneficiaryTokenAccount)).amount;
    assert.equal(
      (beneficiaryBalanceAfter - beneficiaryBalanceBefore).toString(),
      expectedVestedAtRevoke.toString()
    );

    // wait past what would have been the full vesting period; nothing more should ever accrue
    await new Promise((resolve) => setTimeout(resolve, (VESTING_SECONDS - CLIFF_SECONDS + 1) * 1000));

    try {
      await program.methods
        .claim()
        .accounts({
          beneficiary: beneficiary.publicKey,
          schedule,
          beneficiaryTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([beneficiary])
        .rpc();
      assert.fail("expected claim to fail after everything vested-at-revoke was already claimed");
    } catch (err) {
      assert.include(String(err), "NothingToClaim");
    }
  });

  it("rejects a double revoke", async () => {
    const schedule = findSchedulePda(new BN(3));
    const vault = findVaultPda(schedule);

    try {
      await program.methods
        .revoke()
        .accounts({
          grantor: grantor.publicKey,
          schedule,
          grantorTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([grantor])
        .rpc();
      assert.fail("expected the second revoke to fail");
    } catch (err) {
      assert.include(String(err), "AlreadyRevoked");
    }
  });

  it("rejects a non-grantor from revoking", async () => {
    const { schedule, vault } = await createSchedule(4, true);

    try {
      await program.methods
        .revoke()
        .accounts({
          grantor: outsider.publicKey,
          schedule,
          grantorTokenAccount,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([outsider])
        .rpc();
      assert.fail("expected revoke to fail for a non-grantor");
    } catch (err) {
      assert.include(String(err), "Unauthorized");
    }
  });

  it("rejects closing a schedule while the vault still holds tokens", async () => {
    const schedule = findSchedulePda(new BN(4));
    const vault = findVaultPda(schedule);

    try {
      await program.methods
        .closeVesting()
        .accounts({
          grantor: grantor.publicKey,
          schedule,
          vault,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([grantor])
        .rpc();
      assert.fail("expected close_vesting to fail while the vault is non-empty");
    } catch (err) {
      assert.include(String(err), "VaultNotEmpty");
    }
  });
});
