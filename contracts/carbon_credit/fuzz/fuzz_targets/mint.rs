#![no_main]
use libfuzzer_sys::fuzz_target;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, symbol_short};
use carbon_credit::{CarbonCredit, CarbonCreditClient};
use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

#[derive(arbitrary::Arbitrary, Debug)]
pub struct MintInput {
    pub actions: Vec<(u8, i128)>, // (account_index, amount)
}

fuzz_target!(|input: MintInput| {
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
        accounts.push(Address::generate(&env));
    }

    let mut expected_supply: i128 = 0;

    for (acc_idx, amount) in input.actions {
        let recipient = &accounts[(acc_idx % 5) as usize];
        
        let pre_balance = credit_client.balance_of(recipient, &project_id);
        
        let res = credit_client.try_mint(recipient, &project_id, &amount);
        
        if amount > 0 {
            // It could overflow total supply or balance
            if res.is_ok() {
                expected_supply = expected_supply.checked_add(amount).unwrap();
                let post_balance = credit_client.balance_of(recipient, &project_id);
                assert_eq!(post_balance, pre_balance + amount);
            }
        } else {
            assert!(res.is_err());
        }

        let actual_supply = credit_client.total_supply(&project_id);
        assert_eq!(actual_supply, expected_supply, "Invariant violated: total_supply mismatch");
        
        let mut sum_balances: i128 = 0;
        for acc in &accounts {
            sum_balances = sum_balances.checked_add(credit_client.balance_of(acc, &project_id)).unwrap_or(sum_balances);
        }
        // Since we only mint to known accounts, sum should equal supply, unless it overflows
        assert_eq!(actual_supply, sum_balances, "Invariant violated: total_supply != sum of balances");
    }
});
