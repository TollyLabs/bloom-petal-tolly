// One `<service id>.json` per catalog entry (e.g. token.risk.v2.json).
petal::route_file!(spec: petal::http_dir_spec(), fallible_list: crate::agentic::services_children());
