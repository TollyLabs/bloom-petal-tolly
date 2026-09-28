petal::route_file!(spec: petal::store_dir_spec().caps(&["bloom:vfs.read"]), ctx_list: |ctx: &petal::Ctx| {
    let wallet = petal::wallet_param(ctx)?;
    let names = petal::sdk::vfs_list(&format!("wallets/{wallet}"), 65536).map_err(|e| petal::error(-4, e.message()))?;
    Ok(petal::dirs(names.into_iter().filter(|n| n.parse::<u32>().is_ok() && (n == "0" || !n.starts_with("0"))).collect()))
});
