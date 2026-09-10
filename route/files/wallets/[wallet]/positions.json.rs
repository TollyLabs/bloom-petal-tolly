// Native USDC (18) and ERC-20 USDC (6) plus the balance of every token this
// wallet's operations touched (bounded; symbols/decimals from the records).
// Balances are 5 s-cached (account spec), not the 30 s store default.
petal::route_file!(
    spec: petal::account_read_spec().caps(&["bloom:store", "bloom:chain", "bloom:vfs.read"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::positions::positions_document(wallet)
    }
);
