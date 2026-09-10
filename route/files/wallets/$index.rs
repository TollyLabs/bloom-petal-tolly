// Wallet ids with at least one operation record. Any Bloom wallet id is
// addressable as wallets/<id>/ whether or not it is listed here.
fn children(_ctx: &petal::Ctx) -> Result<Vec<petal::RouteChild>, petal::DispatchResponse> {
    crate::ops::list_wallets()
        .map(petal::dirs)
        .map_err(|error| crate::err(-4, error))
}

petal::route_file!(spec: petal::store_dir_spec().caps(&["bloom:store"]), ctx_list: children);
