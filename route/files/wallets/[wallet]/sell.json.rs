// Stage a token -> USDC sell (one transaction per write, no interface fee).
petal::route_file!(
    spec: petal::write_spec().caps(&["bloom:http", "bloom:store", "bloom:tx.outbox", "bloom:chain", "bloom:vfs.read"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::swap::sell_description(wallet)
    },
    write: |ctx: &petal::Ctx, body: &[u8]| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::swap::route_sell(wallet, body)
    }
);
