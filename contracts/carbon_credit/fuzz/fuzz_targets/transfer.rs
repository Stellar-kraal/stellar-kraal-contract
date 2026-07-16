#![no_main]
use libfuzzer_sys::fuzz_target;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, symbol_short};
use carbon_credit::{CarbonCredit, CarbonCreditClient};
use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

#[derive(arbitrary::Arbitrary, Debug)]
pub struct TransferInput {
    pub actions: Vec<(u8, u8, i128)>, // (from_index, to_index, amount)
}

fuzz_target!(|input: TransferInput| {
    let env = Env::default();
    env.mock_all_auths();

    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id = reg_client.register_project(&owner, &symbol_short!("FUZZ"), &i128::MAX, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let mut accounts = Vec::new();
    for _ in 0..5 {
        let acc = Address::generate(&env);
        let _ = credit_client.try_mint(&acc, &project_id, &1_000_000_000_i128);
        accounts.push(acc);
    }

    let initial_supply = credit_client.total_supply(&project_id);

    for (from_idx, to_idx, amount) in input.actions {
        let from_acc = &accounts[(from_idx % 5) as usize];
        let to_acc = &accounts[(to_idx % 5) as usize];
        
        let _res = credit_client.try_transfer(from_acc, to_acc, &project_id, &amount);
        
        let current_supply = credit_client.total_supply(&project_id);
        // Invariant: transfer doesn't change total supply
        assert_eq!(current_supply, initial_supply, "Invariant violated: total supply changed during transfer");

        let mut sum_balances: i128 = 0;
        for acc in &accounts {
            sum_balances = sum_balances.checked_add(credit_client.balance_of(acc, &project_id)).unwrap_or(sum_balances);
        }
        assert_eq!(current_supply, sum_balances, "Invariant violated: total_supply != sum of balances");
    }
});
