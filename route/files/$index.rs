petal::route_file!(
    spec: petal::static_dir_spec(),
    list: {
        let mut children = petal::files(&["README.md", "AGENTS.md", "status.json", "markets.json"]);
        children.extend(petal::dir_names(&["tokens", "quote", "wallets"]));
        children
    }
);
