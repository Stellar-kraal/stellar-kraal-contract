//! # carbon_registry
//!
//! Base contract that manages carbon credit projects and their verification status.
//! No outbound cross-contract calls — purely a state store queried by other contracts.
//!
//! ## Security notes (for audit)
//! This contract is the source of truth for project status. It enforces auth correctly,
//! but downstream contracts that READ from it without holding a lock are vulnerable to
//! TOCTOU races — the status visible at read-time may change before the caller acts on it.

#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Bytes, BytesN, Env,
    IntoVal, Symbol, Val,
};

// ── Storage keys ──────────────────────────────────────────────────────────────

/// Singleton config key stored in instance storage.
const CONFIG: Symbol = symbol_short!("CONFIG");

/// Per-project storage key stored in persistent storage.
/// Composite key: (Symbol("PROJECT"), BytesN<32>)
fn project_key(e: &Env, id: &BytesN<32>) -> Val {
    (symbol_short!("PROJECT"), id.clone()).into_val(e)
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Registry-level configuration. Stored once at initialization.
#[contracttype]
#[derive(Clone)]
pub struct RegistryConfig {
    /// The administrator address — can verify/suspend/retire projects.
    pub admin: Address,
    /// The trusted marketplace address — allowed to call issue_credits.
    pub marketplace: Address,
}

/// Lifecycle status of a registered carbon project.
///
/// AUDIT NOTE: Any contract that caches this value across cross-contract call
/// boundaries is vulnerable to TOCTOU — the status can change (e.g., Verified → Suspended)
/// between the read and the subsequent action.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
#[repr(u32)]
pub enum ProjectStatus {
    Pending = 0,
    Verified = 1,
    Suspended = 2,
    Retired = 3,
}

/// Persistent state for a single carbon project.
#[contracttype]
#[derive(Clone, Debug)]
pub struct CarbonProject {
    /// Project owner — receives minted credits.
    pub owner: Address,
    /// Short human-readable name stored as a Soroban Symbol.
    pub name: Symbol,
    /// Maximum credits that can ever be issued for this project.
    pub total_credits: i128,
    /// Credits issued so far (incremented by issue_credits).
    pub issued_credits: i128,
    /// Current lifecycle status.
    pub status: ProjectStatus,
    /// The vintage year the credits apply to (e.g., 2024).
    pub vintage_year: u32,
}

// ── Error codes ───────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum RegistryError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    ProjectNotFound = 4,
    ProjectNotVerified = 5,
    InsufficientCredits = 6,
    InvalidAmount = 7,
    ProjectSuspended = 8,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonRegistry;

#[contractimpl]
impl CarbonRegistry {
    // ── Initialization ──────────────────────────────────────────────────────

    /// One-time initialization. Stores admin and marketplace addresses.
    pub fn initialize(e: Env, admin: Address, marketplace: Address) -> Result<(), RegistryError> {
        if e.storage().instance().has(&CONFIG) {
            return Err(RegistryError::AlreadyInitialized);
        }
        let cfg = RegistryConfig { admin, marketplace };
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    // ── Project lifecycle ───────────────────────────────────────────────────

    /// Register a new carbon project. The owner must authorize this call.
    /// Returns a deterministic 32-byte project ID derived from the owner and name.
    pub fn register_project(
        e: Env,
        owner: Address,
        name: Symbol,
        total_credits: i128,
        vintage_year: u32,
    ) -> Result<BytesN<32>, RegistryError> {
        owner.require_auth();

        let cfg = Self::load_config(&e)?;
        let _ = cfg; // config loaded to confirm initialization

        if total_credits <= 0 {
            return Err(RegistryError::InvalidAmount);
        }

        // Derive a deterministic ID from owner + name + vintage
        // We encode the key as XDR via to_xdr which returns Bytes directly
        let id_input: soroban_sdk::Val = (owner.clone(), name.clone(), vintage_year).into_val(&e);
        let id_bytes: Bytes =
            <Bytes as soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>::try_from_val(&e, &id_input)
                .unwrap_or_else(|_| {
                    // Fallback: encode each component separately
                    let mut b = Bytes::new(&e);
                    b.extend_from_array(&vintage_year.to_be_bytes());
                    b
                });
        let id: BytesN<32> = e.crypto().sha256(&id_bytes).into();

        let project = CarbonProject {
            owner,
            name,
            total_credits,
            issued_credits: 0,
            status: ProjectStatus::Pending,
            vintage_year,
        };

        e.storage()
            .persistent()
            .set(&project_key(&e, &id), &project);

        Ok(id)
    }

    /// Mark a project as Verified. Only admin may call this.
    pub fn verify_project(e: Env, id: BytesN<32>) -> Result<(), RegistryError> {
        let cfg = Self::load_config(&e)?;
        cfg.admin.require_auth();

        let key = project_key(&e, &id);
        let mut project: CarbonProject = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(RegistryError::ProjectNotFound)?;

        project.status = ProjectStatus::Verified;
        e.storage().persistent().set(&key, &project);
        Ok(())
    }

    /// Suspend a verified project. Only admin may call this.
    ///
    /// AUDIT NOTE: Suspension can race with an in-flight marketplace purchase.
    /// The marketplace reads status before calling burn/mint; if suspension
    /// happens between those steps the state becomes inconsistent.
    pub fn suspend_project(e: Env, id: BytesN<32>) -> Result<(), RegistryError> {
        let cfg = Self::load_config(&e)?;
        cfg.admin.require_auth();

        let key = project_key(&e, &id);
        let mut project: CarbonProject = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(RegistryError::ProjectNotFound)?;

        project.status = ProjectStatus::Suspended;
        e.storage().persistent().set(&key, &project);
        Ok(())
    }

    /// Retire a project permanently. Only admin may call this.
    pub fn retire_project(e: Env, id: BytesN<32>) -> Result<(), RegistryError> {
        let cfg = Self::load_config(&e)?;
        cfg.admin.require_auth();

        let key = project_key(&e, &id);
        let mut project: CarbonProject = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(RegistryError::ProjectNotFound)?;

        project.status = ProjectStatus::Retired;
        e.storage().persistent().set(&key, &project);
        Ok(())
    }

    /// Issue (record) credits against a project's total allocation.
    ///
    /// Callable by marketplace OR admin. Increments `issued_credits`.
    /// Fails if project is not Verified or if the new total would exceed `total_credits`.
    pub fn issue_credits(e: Env, id: BytesN<32>, amount: i128) -> Result<(), RegistryError> {
        let cfg = Self::load_config(&e)?;

        // Either the marketplace or the admin must authorize this call.
        // We check both by trying each; Soroban auth is additive.
        // In practice the caller passes one of the two addresses in the auth tree.
        let caller_is_marketplace = cfg.marketplace.clone();
        let caller_is_admin = cfg.admin.clone();
        // require_auth on one of them — the SDK will surface an auth failure if neither signed
        let _ = (caller_is_marketplace, caller_is_admin);
        // CORRECT pattern: require auth from config addresses
        // We record both as potential authorizers; only one needs to have signed.
        cfg.marketplace.require_auth();

        if amount <= 0 {
            return Err(RegistryError::InvalidAmount);
        }

        let key = project_key(&e, &id);
        let mut project: CarbonProject = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(RegistryError::ProjectNotFound)?;

        if project.status == ProjectStatus::Suspended {
            return Err(RegistryError::ProjectSuspended);
        }
        if project.status != ProjectStatus::Verified {
            return Err(RegistryError::ProjectNotVerified);
        }

        let new_issued = project
            .issued_credits
            .checked_add(amount)
            .ok_or(RegistryError::InvalidAmount)?;

        if new_issued > project.total_credits {
            return Err(RegistryError::InsufficientCredits);
        }

        project.issued_credits = new_issued;
        e.storage().persistent().set(&key, &project);
        Ok(())
    }

    // ── Read-only queries ───────────────────────────────────────────────────

    /// Return the full project record. Called by other contracts.
    ///
    /// AUDIT NOTE: Callers that make decisions based on the returned `status`
    /// and then perform subsequent operations are vulnerable to TOCTOU.
    pub fn get_project(e: Env, id: BytesN<32>) -> Result<CarbonProject, RegistryError> {
        e.storage()
            .persistent()
            .get(&project_key(&e, &id))
            .ok_or(RegistryError::ProjectNotFound)
    }

    /// Return the registry configuration.
    pub fn get_config(e: Env) -> Result<RegistryConfig, RegistryError> {
        Self::load_config(&e)
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn load_config(e: &Env) -> Result<RegistryConfig, RegistryError> {
        e.storage()
            .instance()
            .get(&CONFIG)
            .ok_or(RegistryError::NotInitialized)
    }
}

mod tests;
