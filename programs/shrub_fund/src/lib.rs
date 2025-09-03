#![allow(unexpected_cfgs)]
#![allow(deprecated)]

use anchor_lang::prelude::*;
use anchor_spl::{
    token_interface::{self, TokenAccount as InterfaceTokenAccount, TokenInterface, Transfer as InterfaceTransfer},
};

declare_id!("GYe1hhxHhojNy5LfTddD79BdHCnsYC2dsD8KMrKn1se6");

const GARDENER_PUBKEY: Pubkey = pubkey!("HBPb3Gvidfji29awKFb24wTgNJqvv75LUwPxgojwZgVU");
const TRADING_WALLET: Pubkey = pubkey!("9Ufh1tSTYzSjwMTAczsPoRhtTKqwUvoVywGqPjDd9cP5");
const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

// Registry system constants
const MAX_USERS_PER_REGISTRY: u64 = 100_000;
const MAX_REGISTRIES: u64 = 100;

// Precision for NAV calculations (6 decimals to match USDC)
const NAV_PRECISION: u64 = 1_000_000;

// NAV History configuration (84 entries for 7 days at 2-hour intervals)
const MAX_NAV_HISTORY: usize = 84;

// Helper functions
fn calculate_optimized_nav_per_share(fund_pool: &Account<FundPool>) -> Result<u64> {
    if fund_pool.total_shares == 0 {
        return Ok(NAV_PRECISION); // Initial NAV = $1.00
    }
    
    let optimized_nav = (fund_pool.optimized_nav as u128)
        .checked_mul(NAV_PRECISION as u128)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(fund_pool.total_shares as u128)
        .ok_or(ErrorCode::MathOverflow)? as u64;
    
    Ok(optimized_nav)
}

fn calculate_real_nav_per_share(fund_pool: &Account<FundPool>) -> Result<u64> {
    if fund_pool.total_shares == 0 {
        return Ok(NAV_PRECISION); // Initial NAV = $1.00
    }
    
    let real_nav = (fund_pool.real_nav as u128)
        .checked_mul(NAV_PRECISION as u128)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(fund_pool.total_shares as u128)
        .ok_or(ErrorCode::MathOverflow)? as u64;
    
    Ok(real_nav)
}

// Calculate shares based on REAL NAV to prevent discount purchases
fn calculate_shares_for_usdc_real_nav(fund_pool: &Account<FundPool>, usdc_amount: u64) -> Result<u64> {
    let real_nav_per_share = calculate_real_nav_per_share(fund_pool)?;
    
    let shares = (usdc_amount as u128)
        .checked_mul(NAV_PRECISION as u128)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(real_nav_per_share as u128)
        .ok_or(ErrorCode::MathOverflow)? as u64;
    
    Ok(shares)
}

fn calculate_usdc_for_shares(fund_pool: &Account<FundPool>, shares: u64) -> Result<u64> {
    let optimized_nav_per_share = calculate_optimized_nav_per_share(fund_pool)?;
    
    let usdc_amount = (shares as u128)
        .checked_mul(optimized_nav_per_share as u128)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(NAV_PRECISION as u128)
        .ok_or(ErrorCode::MathOverflow)? as u64;
    
    Ok(usdc_amount)
}

// Calculate average NAV from history entries
fn calculate_average_nav(nav_history: &Vec<NavHistoryEntry>) -> Result<u64> {
    if nav_history.is_empty() {
        return Err(ErrorCode::EmptyNavHistory.into());
    }
    
    let sum: u128 = nav_history.iter().map(|entry| entry.real_nav as u128).sum();
    let average = sum / nav_history.len() as u128;
    
    Ok(average as u64)
}

fn update_nav_history(nav_history: &mut Account<NavHistory>, new_real_nav: u64, current_time: i64) -> Result<Vec<u64>> {
    // Add new entry
    let new_entry = NavHistoryEntry {
        timestamp: current_time,
        real_nav: new_real_nav,
    };
    
    nav_history.entries.push(new_entry);
    
    // Remove entries older than 7 days (for 2-hour updates, max 84 entries)
    let seven_days_ago = current_time - (7 * 24 * 60 * 60);
    nav_history.entries.retain(|entry| entry.timestamp >= seven_days_ago);
    
    // If history is too long, remove oldest entries
    while nav_history.entries.len() > MAX_NAV_HISTORY {
        nav_history.entries.remove(0);
    }
    
    // Return all real_nav values for averaging
    let real_navs: Vec<u64> = nav_history.entries.iter().map(|entry| entry.real_nav).collect();
    Ok(real_navs)
}

// Registry helper functions
fn registry_add_user<'a>(
    registry_directory: &mut Account<RegistryDirectory>,
    user_registry: &mut Account<'a, UserRegistry>,
    user: Pubkey,
    shares: u64,
    stake_timestamp: i64,
    payer: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
) -> Result<(u64, u64)> {
    // Program-level fallback: Check if registry is full before attempting to add
    if user_registry.users.len() >= MAX_USERS_PER_REGISTRY as usize {
        msg!("Registry {} is full ({} users). Frontend should try a different registry.", 
             user_registry.registry_id, user_registry.users.len());
        return Err(ErrorCode::RegistryFull.into());
    }
    
    let user_info = UserInfo {
        user_pubkey: user,
        shares,
        stake_timestamp,
        unstake_initialized_timestamp: None,
        unstake_shares: None,
        unstake_usdc_value: None,
    };
    
    // Try to find empty slot first (reuse gaps from completed unstakes)
    let mut found_index = None;
    for (i, slot) in user_registry.users.iter().enumerate() {
        if slot.is_none() {
            found_index = Some(i);
            break;
        }
    }
    
    let registry_index = match found_index {
        Some(index) => {
            // Reuse empty slot
            user_registry.users[index] = Some(user_info);
            index
        }
        None => {
            // No gaps found, need to extend the array
            let current_data_len = user_registry.to_account_info().data_len();
            let user_info_size = 32 + 8 + 8 + 9 + 9 + 9; // Pubkey + u64 + i64 + 3 Options
            let additional_space = 1 + user_info_size; // 1 byte for Option discriminator + UserInfo size
            let new_data_len = current_data_len + additional_space;
            
            // Reallocate account
            user_registry.to_account_info().realloc(new_data_len, false)?;
            
            // Transfer lamports for rent
            let rent = Rent::get()?;
            let rent_exempt_lamports = rent.minimum_balance(new_data_len);
            let current_lamports = user_registry.to_account_info().lamports();
            
            if rent_exempt_lamports > current_lamports {
                let additional_lamports = rent_exempt_lamports - current_lamports;
                
                anchor_lang::system_program::transfer(
                    CpiContext::new(
                        system_program.clone(),
                        anchor_lang::system_program::Transfer {
                            from: payer.clone(),
                            to: user_registry.to_account_info(),
                        },
                    ),
                    additional_lamports,
                )?;
            }
            
            // Extend array
            user_registry.users.push(Some(user_info));
            user_registry.users.len() - 1
        }
    };
    
    // Update registry counters
    user_registry.user_count = user_registry.user_count.checked_add(1).ok_or(ErrorCode::MathOverflow)?;
    
    // Update directory
    registry_directory.active_users = registry_directory.active_users.checked_add(1).ok_or(ErrorCode::MathOverflow)?;
    
    Ok((user_registry.registry_id, registry_index as u64))
}

#[program]
pub mod shrub_fund {
    use super::*;



    // === MAIN PROGRAM FUNCTIONS ===

    pub fn initialize_user_registry(ctx: Context<InitializeUserRegistry>, registry_id: u64) -> Result<()> {
        let registry_directory = &mut ctx.accounts.registry_directory;
        let user_registry = &mut ctx.accounts.user_registry;
        
        require!(registry_id < MAX_REGISTRIES, ErrorCode::InvalidRegistryIndex);
        require!(registry_directory.total_registries < MAX_REGISTRIES, ErrorCode::RegistryFull);
        
        // Initialize registry
        user_registry.registry_id = registry_id;
        user_registry.user_count = 0;
        user_registry.users = Vec::new();
        
        // Update directory
        registry_directory.total_registries = registry_directory.total_registries
            .checked_add(1)
            .ok_or(ErrorCode::MathOverflow)?;
        
        msg!("User registry {} initialized", registry_id);
        Ok(())
    }

    pub fn stake_usdc(ctx: Context<StakeUsdc>, usdc_amount: u64) -> Result<()> {
        let fund_pool = &mut ctx.accounts.fund_pool;
        let user_share = &mut ctx.accounts.user_share;
        let clock = Clock::get()?;

        // Calculate shares based on REAL NAV to prevent discount purchases
        let shares = calculate_shares_for_usdc_real_nav(fund_pool, usdc_amount)?;
        require!(shares > 0, ErrorCode::InsufficientAmount);

        // Transfer USDC from user to trading wallet
        token_interface::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                InterfaceTransfer {
                    from: ctx.accounts.user_usdc_account.to_account_info(),
                    to: ctx.accounts.trading_usdc_account.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            usdc_amount,
        )?;

        // Track if this is user's first stake
        let is_first_stake = user_share.user == Pubkey::default();

        // Initialize user share if first time
        if is_first_stake {
            user_share.user = ctx.accounts.user.key();
            user_share.shares = 0;
            user_share.stake_timestamp = clock.unix_timestamp;
            fund_pool.total_users = fund_pool.total_users
                .checked_add(1)
                .ok_or(ErrorCode::MathOverflow)?;
        }

        // Update user and fund totals
        user_share.shares = user_share.shares.checked_add(shares).ok_or(ErrorCode::MathOverflow)?;
        fund_pool.total_shares = fund_pool.total_shares.checked_add(shares).ok_or(ErrorCode::MathOverflow)?;
        
        // Add USDC amount to real_nav since USDC is being transferred in
        fund_pool.real_nav = fund_pool.real_nav.checked_add(usdc_amount).ok_or(ErrorCode::MathOverflow)?;

        // Registry Integration: Add user to central registry
        if is_first_stake {
            if let Some(registry_directory) = &mut ctx.accounts.registry_directory {
                if let Some(user_registry) = &mut ctx.accounts.user_registry {
                    let (registry_id, registry_index) = registry_add_user(
                        registry_directory,
                        user_registry,
                        ctx.accounts.user.key(),
                        user_share.shares,
                        user_share.stake_timestamp,
                        &ctx.accounts.user.to_account_info(),
                        &ctx.accounts.system_program.to_account_info(),
                    )?;
                    user_share.registry_id = registry_id;
                    user_share.registry_index = registry_index;
                } 
            }
        }

        let current_optimized_nav = calculate_optimized_nav_per_share(fund_pool)?;
        let current_real_nav = calculate_real_nav_per_share(fund_pool)?;
        msg!("Staked {} USDC for {} shares. User: {}. Real NAV: ${:.6}, Optimized NAV: ${:.6}", 
             usdc_amount, shares, user_share.user, 
             current_real_nav as f64 / NAV_PRECISION as f64,
             current_optimized_nav as f64 / NAV_PRECISION as f64);
        Ok(())
    }

    pub fn initiate_unstake(ctx: Context<InitiateUnstake>) -> Result<()> {
        let user_share = &mut ctx.accounts.user_share;
        let fund_pool = &mut ctx.accounts.fund_pool;
        let pending_cashout_pool = &mut ctx.accounts.pending_cashout_pool;
        let user_registry = &mut ctx.accounts.user_registry;
        let clock = Clock::get()?;

        require!(user_share.shares > 0, ErrorCode::NoShares);
        require!(!user_share.unstake_initiated, ErrorCode::UnstakeAlreadyPending);

        // Calculate and LOCK USDC value at current optimized NAV
        let locked_usdc_value = calculate_usdc_for_shares(fund_pool, user_share.shares)?;

        // Update user registry with unstake info
        if let Some(user_info) = &mut user_registry.users[user_share.registry_index as usize] {
            user_info.unstake_initialized_timestamp = Some(clock.unix_timestamp);
            user_info.unstake_shares = Some(user_share.shares);
            user_info.unstake_usdc_value = Some(locked_usdc_value);
        }

        // Mark user as having unstake initiated
        user_share.unstake_initiated = true;

        // Add to pending cashout pool - expand if needed
        let pending_user = PendingUsers {
            user: ctx.accounts.user.key(),
            pending_usdc_cashout: locked_usdc_value,
        };
        
        // Check if we need to expand the PDA (gap filling happens automatically via retain())
        if pending_cashout_pool.users.len() >= pending_cashout_pool.users.capacity() {
            // This will trigger a realloc to expand the account
            // The user (signer) pays for the additional space
            msg!("Expanding Pending Cashout Pool capacity for new unstake request");
        }
        
        pending_cashout_pool.users.push(pending_user);

        // Update fund pool
        fund_pool.pending_cashout = fund_pool.pending_cashout
            .checked_add(locked_usdc_value)
            .ok_or(ErrorCode::MathOverflow)?;

        // Immediately subtract both shares and value from fund pool
        fund_pool.total_shares = fund_pool.total_shares
            .checked_sub(user_share.shares)
            .ok_or(ErrorCode::MathOverflow)?;
        fund_pool.real_nav = fund_pool.real_nav
            .checked_sub(locked_usdc_value)
            .ok_or(ErrorCode::MathOverflow)?;

        msg!("Unstake initiated: {} shares, completion in 7 days. Locked value: {} USDC", 
             user_share.shares, locked_usdc_value);
        Ok(())
    }

    pub fn complete_unstake(ctx: Context<CompleteUnstake>) -> Result<()> {
        let user_share = &mut ctx.accounts.user_share;
        let fund_pool = &mut ctx.accounts.fund_pool;
        let pending_cashout_pool = &mut ctx.accounts.pending_cashout_pool;
        let user_registry = &mut ctx.accounts.user_registry;
        let clock = Clock::get()?;

        require!(user_share.unstake_initiated, ErrorCode::NoUnstakePending);
        
        // Check 7-day waiting period
        let user_info = &user_registry.users[user_share.registry_index as usize]
            .as_ref()
            .ok_or(ErrorCode::UserNotInRegistry)?;
        
        let seven_days = 7 * 24 * 60 * 60;
        require!(
            clock.unix_timestamp >= user_info.unstake_initialized_timestamp.unwrap() + seven_days,
            ErrorCode::UnstakeNotReady
        );

        // Use the locked USDC value from initiation
        let final_usdc_amount = user_info.unstake_usdc_value.unwrap();
        require!(final_usdc_amount > 0, ErrorCode::InsufficientFundValue);

        // Transfer USDC from pending cashout pool PDA to user
        let pending_cashout_seeds = &[
            b"pending_cashout_pool_v2".as_ref(),
            &[ctx.bumps.pending_cashout_pool],
        ];
        token_interface::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                InterfaceTransfer {
                    from: ctx.accounts.pending_cashout_usdc_account.to_account_info(),
                    to: ctx.accounts.user_usdc_account.to_account_info(),
                    authority: pending_cashout_pool.to_account_info(),
                },
                &[pending_cashout_seeds],
            ),
            final_usdc_amount,
        )?;

        // Clear user from registry
        user_registry.users[user_share.registry_index as usize] = None;
        user_registry.user_count = user_registry.user_count
            .checked_sub(1)
            .ok_or(ErrorCode::MathOverflow)?;

        // Remove from pending cashout pool
        pending_cashout_pool.users.retain(|u| u.user != ctx.accounts.user.key());
        fund_pool.pending_cashout = fund_pool.pending_cashout
            .checked_sub(final_usdc_amount)
            .ok_or(ErrorCode::MathOverflow)?;

        // Update fund totals (shares already subtracted in initiate_unstake)
        fund_pool.total_users = fund_pool.total_users
            .checked_sub(1)
            .ok_or(ErrorCode::MathOverflow)?;

        // Clear user share
        user_share.user = Pubkey::default();
        user_share.shares = 0;
        user_share.unstake_initiated = false;

        msg!("Unstake completed: {} USDC transferred to user", final_usdc_amount);
        Ok(())
    }

    pub fn update_optimized_nav(ctx: Context<UpdateOptimizedNav>, new_portfolio_value: u64) -> Result<()> {
        // Only gardener (Lambda) can update NAV
        require!(
            ctx.accounts.gardener.key() == GARDENER_PUBKEY,
            ErrorCode::UnauthorizedGardener
        );

        let fund_pool = &mut ctx.accounts.fund_pool;
        let nav_history = &mut ctx.accounts.nav_history;
        let pending_cashout_pool = &ctx.accounts.pending_cashout_pool;
        let clock = Clock::get()?;

        let old_optimized_nav = fund_pool.optimized_nav;
        
        // Always update real NAV immediately
        fund_pool.real_nav = new_portfolio_value;
        
        // Update NAV history and get all real_nav values for averaging
        let _real_navs = update_nav_history(nav_history, new_portfolio_value, clock.unix_timestamp)?;
        let calculated_optimized_nav = calculate_average_nav(&nav_history.entries)?;
        
        // Apply one-directional averaged NAV system logic
        let final_optimized_nav = if new_portfolio_value < old_optimized_nav {
            // NAV decrease: apply immediately
            new_portfolio_value
        } else {
            // NAV increase: use averaged NAV
            calculated_optimized_nav
        };
        
        fund_pool.optimized_nav = final_optimized_nav;

        // Calculate total pending cashout for alert
        let total_pending: u64 = pending_cashout_pool.users.iter()
            .map(|u| u.pending_usdc_cashout)
            .sum();
        fund_pool.pending_cashout = total_pending;

        if total_pending > 0 {
            msg!("ALERT: {} USDC needed for pending cashouts", total_pending);
        }

        msg!("NAV updated: Optimized {} USDC, Real {} USDC", 
             final_optimized_nav, new_portfolio_value);
        Ok(())
    }

    // View function to get user info
    pub fn get_user_info(ctx: Context<GetUserInfo>) -> Result<()> {
        let user_share = &ctx.accounts.user_share;
        let fund_pool = &ctx.accounts.fund_pool;
        
        if user_share.shares > 0 {
            let current_value = calculate_usdc_for_shares(fund_pool, user_share.shares)?;
            let optimized_nav_per_share = calculate_optimized_nav_per_share(fund_pool)?;
            
            msg!("User Share Info:");
            msg!("  User: {}", user_share.user);
            msg!("  Shares: {}", user_share.shares);
            msg!("  Current Value: ${:.2} USDC", current_value as f64 / NAV_PRECISION as f64);
            msg!("  NAV per Share: ${:.6}", optimized_nav_per_share as f64 / NAV_PRECISION as f64);
            msg!("  Registry ID: {}", user_share.registry_id);
            msg!("  Registry Index: {}", user_share.registry_index);
        } else {
            msg!("User has no shares in the fund");
        }
        
        Ok(())
    }

    // Monthly commission collection (triggered by Lambda)
    pub fn collect_monthly_commission(ctx: Context<CollectMonthlyCommission>) -> Result<()> {
        let fund_pool = &mut ctx.accounts.fund_pool;
        let current_time = Clock::get()?.unix_timestamp;
        
        msg!("📊 Monthly Commission Collection - Timestamp: {}", current_time);
        msg!("💰 Fund Status Check:");
        msg!("   Real NAV: {} USDC", fund_pool.real_nav);
        msg!("   Total Shares: {} (represents {} USDC invested)", fund_pool.total_shares, fund_pool.total_shares);
        msg!("   Total Users: {}", fund_pool.total_users);
        
        // 1. Check if fund is profitable using real_nav (current portfolio value)
        if fund_pool.real_nav <= fund_pool.total_shares {
            let current_share_price = if fund_pool.total_shares > 0 {
                (fund_pool.real_nav as u128 * NAV_PRECISION as u128 / fund_pool.total_shares as u128) as u64
            } else {
                NAV_PRECISION
            };
            
            msg!("⏸️ No profit in fund - Current share price: ${:.6}", 
                 current_share_price as f64 / NAV_PRECISION as f64);
            msg!("⏸️ No commission collected. Fund value ({}) ≤ Total invested ({})", 
                 fund_pool.real_nav, fund_pool.total_shares);
            return Ok(());
        }
        
        // 2. Calculate total profit (current value - original investments)
        let total_current_value = fund_pool.real_nav;
        let total_invested = fund_pool.total_shares; // Each share = 1 USDC originally invested
        
        let total_profit = total_current_value
            .checked_sub(total_invested)
            .ok_or(ErrorCode::MathOverflow)?;
        
        // 3. Calculate 2% commission on total profit only (200 basis points)
        let commission = (total_profit as u128)
            .checked_mul(200)  // 2% = 200 basis points
            .ok_or(ErrorCode::MathOverflow)?
            .checked_div(10000) // Convert basis points to percentage
            .ok_or(ErrorCode::MathOverflow)? as u64;
        
        if commission == 0 {
            msg!("⏸️ Commission rounds to zero - profit too small");
            return Ok(());
        }
        
        // 4. Check available balance in trading wallet
        let available_balance = ctx.accounts.trading_usdc_account.amount;
        
        if available_balance < commission {
            msg!("⚠️ Insufficient trading wallet balance for commission");
            msg!("   Commission due: {} USDC", commission);
            msg!("   Available balance: {} USDC", available_balance);
            return Err(ErrorCode::InsufficientFunds.into());
        }
        
        msg!("💰 Commission Calculation:");
        msg!("   Total Fund Value (real_nav): {} USDC", total_current_value);
        msg!("   Total Originally Invested: {} USDC", total_invested);
        msg!("   Total Profit: {} USDC", total_profit);
        msg!("   Commission (2% of profit): {} USDC", commission);
        
        // 5. Calculate share price impact for transparency
        let pre_commission_share_price = if fund_pool.total_shares > 0 {
            (total_current_value as u128 * NAV_PRECISION as u128 / fund_pool.total_shares as u128) as u64
        } else {
            NAV_PRECISION
        };
        
        let post_commission_value = total_current_value
            .checked_sub(commission)
            .ok_or(ErrorCode::MathOverflow)?;
            
        let post_commission_share_price = if fund_pool.total_shares > 0 {
            (post_commission_value as u128 * NAV_PRECISION as u128 / fund_pool.total_shares as u128) as u64
        } else {
            NAV_PRECISION
        };
        
        msg!("📈 Share Price Impact:");
        msg!("   Before Commission: ${:.6}", pre_commission_share_price as f64 / NAV_PRECISION as f64);
        msg!("   After Commission: ${:.6}", post_commission_share_price as f64 / NAV_PRECISION as f64);
        
        // 6. Transfer commission from trading wallet to gardener
        token_interface::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                InterfaceTransfer {
                    from: ctx.accounts.trading_usdc_account.to_account_info(),
                    to: ctx.accounts.gardener_usdc_account.to_account_info(),
                    authority: ctx.accounts.trading_wallet.to_account_info(),
                },
            ),
            commission,
        )?;
        
        // 7. Update fund NAVs to reflect commission payment
        // Both real_nav and optimized_nav are reduced by commission
        fund_pool.real_nav = fund_pool.real_nav
            .checked_sub(commission)
            .ok_or(ErrorCode::MathOverflow)?;
            
        fund_pool.optimized_nav = fund_pool.optimized_nav
            .checked_sub(commission)
            .ok_or(ErrorCode::MathOverflow)?;
        
        msg!("✅ SUCCESS: Monthly commission collected!");
        msg!("   💸 {} USDC transferred to Gardener", commission);
        msg!("   📊 Updated Real NAV: {} USDC", fund_pool.real_nav);
        msg!("   📊 Updated Optimized NAV: {} USDC", fund_pool.optimized_nav);
        msg!("   🎯 Commission represents {:.3}% of total fund value", 
             commission as f64 / total_current_value as f64 * 100.0);
        
        Ok(())
    }
}

// === ACCOUNT VALIDATION CONTEXTS ===


// Main program contexts
#[derive(Accounts)]
#[instruction(registry_id: u64)]
pub struct InitializeUserRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    
    #[account(
        mut,
        seeds = [b"registry_directory_v2"],
        bump
    )]
    pub registry_directory: Account<'info, RegistryDirectory>,
    
    #[account(
        init,
        payer = authority,
        seeds = [b"user_registry_v2", registry_id.to_le_bytes().as_ref()],
        bump,
        space = 8 + 8 + 8 + 4 // Just registry metadata, no users initially
    )]
    pub user_registry: Account<'info, UserRegistry>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(usdc_amount: u64)]
pub struct StakeUsdc<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, token::mint = USDC_MINT, token::authority = user)]
    pub user_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    #[account(
        init_if_needed,
        payer = user,
        seeds = [b"user_share_v2", user.key().as_ref()],
        bump,
        space = 8 + UserShare::INIT_SPACE
    )]
    pub user_share: Account<'info, UserShare>,
    #[account(mut, seeds = [b"optimized_fund_pool_v2"], bump)]
    pub fund_pool: Account<'info, FundPool>,
    /// CHECK: Trading wallet - verified by address constraint
    #[account(address = TRADING_WALLET)]
    pub trading_wallet: UncheckedAccount<'info>,
    #[account(mut, token::mint = USDC_MINT, token::authority = trading_wallet)]
    pub trading_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    
    // Registry accounts (optional - for new users)
    #[account(mut, seeds = [b"registry_directory_v2"], bump)]
    pub registry_directory: Option<Account<'info, RegistryDirectory>>,
    
    #[account(mut)]
    pub user_registry: Option<Account<'info, UserRegistry>>,
}

#[derive(Accounts)]
pub struct InitiateUnstake<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"user_share_v2", user.key().as_ref()], bump)]
    pub user_share: Account<'info, UserShare>,
    #[account(mut, seeds = [b"optimized_fund_pool_v2"], bump)]
    pub fund_pool: Account<'info, FundPool>,
    #[account(
        mut,
        seeds = [b"pending_cashout_pool_v2"],
        bump,
        realloc = 8 + PendingCashoutPool::INIT_SPACE + (pending_cashout_pool.users.len().saturating_sub(100)) * std::mem::size_of::<PendingUsers>(),
        realloc::payer = user,
        realloc::zero = false
    )]
    pub pending_cashout_pool: Account<'info, PendingCashoutPool>,
    #[account(mut)]
    pub user_registry: Account<'info, UserRegistry>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CompleteUnstake<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, token::mint = USDC_MINT, token::authority = user)]
    pub user_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    #[account(mut, seeds = [b"user_share_v2", user.key().as_ref()], bump)]
    pub user_share: Account<'info, UserShare>,
    #[account(mut, seeds = [b"optimized_fund_pool_v2"], bump)]
    pub fund_pool: Account<'info, FundPool>,
    #[account(mut, seeds = [b"pending_cashout_pool_v2"], bump)]
    pub pending_cashout_pool: Account<'info, PendingCashoutPool>,
    #[account(mut, token::mint = USDC_MINT, token::authority = pending_cashout_pool)]
    pub pending_cashout_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    #[account(mut)]
    pub user_registry: Account<'info, UserRegistry>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(new_portfolio_value: u64)]
pub struct UpdateOptimizedNav<'info> {
    #[account(mut)]
    pub gardener: Signer<'info>,
    #[account(mut, seeds = [b"optimized_fund_pool_v2"], bump)]
    pub fund_pool: Account<'info, FundPool>,
    #[account(mut, seeds = [b"nav_history_v2"], bump)]
    pub nav_history: Account<'info, NavHistory>,
    #[account(seeds = [b"pending_cashout_pool_v2"], bump)]
    pub pending_cashout_pool: Account<'info, PendingCashoutPool>,
}

#[derive(Accounts)]
pub struct GetUserInfo<'info> {
    pub user: Signer<'info>,
    #[account(seeds = [b"user_share_v2", user.key().as_ref()], bump)]
    pub user_share: Account<'info, UserShare>,
    #[account(seeds = [b"optimized_fund_pool_v2"], bump)]
    pub fund_pool: Account<'info, FundPool>,
}

#[derive(Accounts)]
pub struct CollectMonthlyCommission<'info> {
    #[account(mut, constraint = gardener.key() == GARDENER_PUBKEY)]
    pub gardener: Signer<'info>,
    
    #[account(mut, seeds = [b"optimized_fund_pool_v2"], bump = 254)]
    pub fund_pool: Account<'info, FundPool>,
    
    /// CHECK: Trading wallet - verified by address constraint
    #[account(address = TRADING_WALLET)]
    pub trading_wallet: UncheckedAccount<'info>,
    
    #[account(mut, token::mint = USDC_MINT, token::authority = trading_wallet)]
    pub trading_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    
    #[account(mut, token::mint = USDC_MINT, token::authority = gardener)]
    pub gardener_usdc_account: InterfaceAccount<'info, InterfaceTokenAccount>,
    
    pub token_program: Interface<'info, TokenInterface>,
}

// === ACCOUNT DATA STRUCTURES ===

#[account]
#[derive(InitSpace)]
pub struct FundPool {
    pub total_shares: u64,
    pub optimized_nav: u64,        // Optimized NAV (one-directional Averaged NAV smoothed)
    pub real_nav: u64,             // live current portfolio value (for share pricing)
    pub total_users: u64,          // sum of all users
    pub pending_cashout: u64,      // sum of all current pending unstakes
    pub authority: Pubkey,         // Gardener pubkey
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct NavHistory {
    #[max_len(84)]
    pub entries: Vec<NavHistoryEntry>, // 7 days of 2-hour intervals
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct NavHistoryEntry {
    pub timestamp: i64,
    pub real_nav: u64,
}

#[account]
#[derive(InitSpace)]
pub struct PendingCashoutPool {
    #[max_len(100)]
    pub users: Vec<PendingUsers>, // Vec of users with pending unstakes
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct PendingUsers {
    pub user: Pubkey,              // user pubkey
    pub pending_usdc_cashout: u64, // USDC amount
}

#[account]
#[derive(InitSpace)]
pub struct UserShare {
    pub user: Pubkey,                 // User's wallet pubkey
    pub shares: u64,                  // Number of shares owned
    pub stake_timestamp: i64,         // Most recent stake timestamp
    pub registry_id: u64,             // Which registry they're in (for 100k scaling)
    pub registry_index: u64,          // Position in UserRegistry.users
    pub unstake_initiated: bool,      // boolean to show if unstake already initiated
}

#[account]
#[derive(InitSpace)]
pub struct RegistryDirectory {
    pub total_registries: u64,        // Number of active registries
    pub active_users: u64,            // Total active users across all registries
}

#[account]
#[derive(InitSpace)]
pub struct UserRegistry {
    pub registry_id: u64,            // Which registry this is (0, 1, 2, etc.)
    pub user_count: u64,             // Users in this registry
    #[max_len(100_000)]
    pub users: Vec<Option<UserInfo>>, // Vec of user info (staking data for quick access)
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct UserInfo {
    pub user_pubkey: Pubkey,                      // User's Public key, serving as unique identifier
    pub shares: u64,                              // Number of shares owned
    pub stake_timestamp: i64,                     // When they first staked
    pub unstake_initialized_timestamp: Option<i64>, // optional entry defaulted to none
    pub unstake_shares: Option<u64>,              // optional entry defaulted to none
    pub unstake_usdc_value: Option<u64>,          // optional entry defaulted to none
}

#[error_code]
pub enum ErrorCode {
    #[msg("Math operation resulted in overflow")]
    MathOverflow,
    #[msg("Minimum 7-day lockup period not met")]
    MinimumLockupNotMet,
    #[msg("No shares owned")]
    NoShares,
    #[msg("Insufficient amount")]
    InsufficientAmount,
    #[msg("Insufficient fund value")]
    InsufficientFundValue,
    #[msg("Only the gardener can update NAV")]
    UnauthorizedGardener,
    #[msg("Unstake request already pending for this user")]
    UnstakeAlreadyPending,
    #[msg("No unstake request pending")]
    NoUnstakePending,
    #[msg("Unstake not ready - 7 day period not complete")]
    UnstakeNotReady,
    #[msg("Update too frequent - minimum 2 hour interval required")]
    UpdateTooFrequent,
    #[msg("Registry is full - cannot add more users")]
    RegistryFull,
    #[msg("Invalid registry index")]
    InvalidRegistryIndex,
    #[msg("User not found in registry")]
    UserNotInRegistry,
    #[msg("NAV history is empty")]
    EmptyNavHistory,
    #[msg("Insufficient funds in trading wallet for commission")]
    InsufficientFunds,
}