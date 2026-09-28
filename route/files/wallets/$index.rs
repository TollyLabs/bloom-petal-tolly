petal::route_file!(spec: petal::store_dir_spec().caps(&["bloom:vfs.read"]), ctx_list: |_ctx: &petal::Ctx| {
    petal::sdk::vfs_list("wallets", 65536).map(|names| petal::dirs(names.into_iter().filter(|n| n != "new" && petal::is_safe_segment(n)).collect())).map_err(|e| petal::error(-4, e.message()))
});
