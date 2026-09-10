// Launch a token on TollyPad (approve for the dev buy when needed, then
// createToken; one transaction per write).
petal::route_file!(
    spec: petal::write_spec().caps(&["bloom:http", "bloom:store", "bloom:tx.outbox", "bloom:chain", "bloom:vfs.read"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::launch::launch_description(wallet)
    },
    write: |ctx: &petal::Ctx, body: &[u8]| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        crate::launch::route_launch(wallet, body)
    }
);
