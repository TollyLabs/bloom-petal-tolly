// Stage a USDC -> token buy (one transaction per write). The read side is
// the body schema, limits, and this wallet's recent buy operations.
petal::route_file!(
    spec: petal::write_spec().caps(&["bloom:http", "bloom:store", "bloom:tx.outbox", "bloom:chain", "bloom:vfs.read"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::swap::buy_description(wallet)
    },
    write: |ctx: &petal::Ctx, body: &[u8]| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::swap::route_buy(wallet, body)
    }
);
