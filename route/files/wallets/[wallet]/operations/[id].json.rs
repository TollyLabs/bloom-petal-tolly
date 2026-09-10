// The operation record, reconciled on every read against Bloom's outbox
// (tx_inspect) and domain evidence (balance delta / creator launch index).
// A read that advances the record rewrites it, hence the uncached,
// side-effecting chain spec (critique M8).
petal::route_file!(
    spec: petal::chain_read_spec().caps(&["bloom:http", "bloom:store", "bloom:tx.outbox", "bloom:chain"]),
    read: |ctx: &petal::Ctx| {
        let wallet = match petal::wallet_param(ctx) {
            Ok(wallet) => wallet,
            Err(response) => return response,
        };
        let id = match petal::param(ctx, "id") {
            Ok(id) => id,
            Err(response) => return response,
        };
        let network = match crate::api::Network::current() {
            Ok(network) => network,
            Err(response) => return response,
        };
        crate::ops::read_operation(wallet, id, network)
    }
);
