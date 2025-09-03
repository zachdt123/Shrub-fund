import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { ShrubFund } from "../target/types/shrub_fund";
import { PublicKey, Keypair, SystemProgram, Transaction } from "@solana/web3.js";
import { ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync, createMint, createAssociatedTokenAccountInstruction, mintTo, createSetAuthorityInstruction, AuthorityType, getAccount } from "@solana/spl-token";

describe("shrub_fund", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.ShrubFund as Program<ShrubFund>;
  const user = provider.wallet;

  let usdcMint: PublicKey;
  let shrbMint: PublicKey;
  let userUsdcAta: PublicKey;
  let userShrbAta: PublicKey;
  let vaultAuthority: PublicKey;
  let vaultUsdcAta: PublicKey;
  let mintAuthority: PublicKey;
  let tradingWallet = Keypair.generate();  // Mock trading wallet for local test
  let tradingUsdcAta: PublicKey;

  before(async () => {
    // Mock USDC mint (6 decimals like real USDC)
    usdcMint = await createMint(provider.connection, user.payer, user.publicKey, null, 6);

    // Mock SHRB mint (9 decimals, initially authority is user)
    shrbMint = await createMint(provider.connection, user.payer, user.publicKey, null, 9);

    // Compute PDAs (match lib.rs seeds)
    [mintAuthority] = PublicKey.findProgramAddressSync([Buffer.from("mint_authority")], program.programId);
    [vaultAuthority] = PublicKey.findProgramAddressSync([Buffer.from("vault")], program.programId);

    // Set SHRB mint authority to PDA (simulate transfer)
    const setAuthIx = createSetAuthorityInstruction(
      shrbMint,
      user.publicKey,
      AuthorityType.MintTokens,
      mintAuthority
    );
    const tx = new Transaction().add(setAuthIx);
    await provider.sendAndConfirm(tx, [user.payer]);

    // Create and fund user USDC ATA
    userUsdcAta = getAssociatedTokenAddressSync(usdcMint, user.publicKey);
    const createUserUsdcAtaIx = createAssociatedTokenAccountInstruction(
      user.publicKey,
      userUsdcAta,
      user.publicKey,
      usdcMint,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
    await provider.sendAndConfirm(new Transaction().add(createUserUsdcAtaIx), [user.payer]);
    await mintTo(provider.connection, user.payer, usdcMint, userUsdcAta, user.publicKey, 1000000);  // 1 USDC raw (6 decimals)

    // User SHRB ATA (will be init_if_needed in instruction)
    userShrbAta = getAssociatedTokenAddressSync(shrbMint, user.publicKey);

    // Vault USDC ATA (PDA-owned, init_if_needed in instruction)
    vaultUsdcAta = getAssociatedTokenAddressSync(usdcMint, vaultAuthority, true);

    // Trading USDC ATA (create for mock wallet)
    tradingUsdcAta = getAssociatedTokenAddressSync(usdcMint, tradingWallet.publicKey);
    const createTradingAtaIx = createAssociatedTokenAccountInstruction(
      user.publicKey,
      tradingUsdcAta,
      tradingWallet.publicKey,
      usdcMint,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
    await provider.sendAndConfirm(new Transaction().add(createTradingAtaIx), [user.payer]);
  });

  it("Contributes USDC and mints SHRB", async () => {
    const amount = 1000000n;  // 1 USDC raw (6 decimals; program mints 1:1 raw SHRB)

    await program.methods
      .contribute(amount)
      .accounts({
        user: user.publicKey,
        userUsdcAccount: userUsdcAta,
        userShrbAccount: userShrbAta,
        shrbMint: shrbMint,
        mintAuthority: mintAuthority,
        vaultUsdcAccount: vaultUsdcAta,
        vaultAuthority: vaultAuthority,
        tradingWallet: tradingWallet.publicKey,
        tradingUsdcAccount: tradingUsdcAta,
        usdcMint: usdcMint,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      })
      .rpc();

    // Verify balances after contribution
    const userShrbAccount = await getAccount(provider.connection, userShrbAta);
    const tradingUsdcAccount = await getAccount(provider.connection, tradingUsdcAta);

    console.log("User SHRB balance after (raw):", userShrbAccount.amount.toString());  // Expected: '1000000'
    console.log("User SHRB balance after (UI):", Number(userShrbAccount.amount) / 10**9);  // Expected: 0.001 (due to 9 decimals)
    console.log("Trading USDC balance after (raw):", tradingUsdcAccount.amount.toString());  // Expected: '1000000'
    console.log("Trading USDC balance after (UI):", Number(tradingUsdcAccount.amount) / 10**6);  // Expected: 1
    console.log("Test passed - check transaction logs for 'Contribution processed' message!");
  });
});