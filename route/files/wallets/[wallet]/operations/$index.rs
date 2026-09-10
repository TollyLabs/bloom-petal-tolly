fn children(ctx: &petal::Ctx) -> Result<Vec<petal::RouteChild>, petal::DispatchResponse> {
    let wallet = petal::wallet_param(ctx)?;
    crate::wallet::check_wallet_id(wallet)?;
    crate::ops::list_ids(wallet)
        .map(|ids| ids.into_iter().map(|id| petal::file(format!("{id}.json"))).collect())
        .map_err(|error| crate::err(-4, error))
}

petal::route_file!(spec: petal::store_dir_spec().caps(&["bloom:store"]), ctx_list: children);
