// The operation record, exactly as stored: a pure store projection under the
// 5 s account cache. It never inspects the outbox, the chain or the API, and
// never saves: Bloom binds outbox inspection to the route that staged the
// entry (tx_inspect compares the entry's execution origin, route id
// included, with the caller's), so reconciliation runs from the read of
// buy.json / sell.json / launch.json. A side-effecting spec would also render
// as an empty file on the NFS mount (st_size 0), so this must stay a plain,
// non-side-effecting read (see README "Host facts").
petal::route_file!(
    spec: petal::account_read_spec().caps(&["bloom:store"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        let id = match petal::param(ctx, "id") {
            Ok(id) => id,
            Err(response) => return response,
        };
        crate::ops::read_operation(wallet, id)
    }
);
