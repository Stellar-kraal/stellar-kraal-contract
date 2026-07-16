#![no_main]
use libfuzzer_sys::fuzz_target;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, symbol_short, vec};
use carbon_credit::{CarbonCredit, CarbonCreditClient};
use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

#[derive(arbitrary::Arbitrary, Debug)]
pub struct BatchAction {
    pub from_idx: u8,
    pub transfers: Vec<(u8, i128)>, // (to_idx, amount)
}

#[derive(arbitrary::Arbitrary, Debug)]
pub struct BatchInput {
    pub actions: Vec<BatchAction>,
}

fuzz_target!(|input: BatchInput| {
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

    for action in input.actions {
        let from_acc = &accounts[(action.from_idx % 5) as usize];
        
        let mut transfers_vec = vec![&env];
        for (to_idx, amount) in action.transfers {
            let to_acc = accounts[(to_idx % 5) as usize].clone();
            transfers_vec.push_back((to_acc, project_id.clone(), amount));
        }

        let _res = credit_client.try_batch_transfer(from_acc, &transfers_vec);
        
        let current_supply = credit_client.total_supply(&project_id);
        assert_eq!(current_supply, initial_supply, "Invariant violated: total supply changed during batch transfer");

        let mut sum_balances: i128 = 0;
        for acc in &accounts {
            sum_balances = sum_balances.checked_add(credit_client.balance_of(acc, &project_id)).unwrap_or(sum_balances);
        }
        assert_eq!(current_supply, sum_balances, "Invariant violated: total_supply != sum of balances");
    }
});
