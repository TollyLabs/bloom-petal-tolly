// One `<agent id>.json` per listed agent.
petal::route_file!(spec: petal::http_dir_spec(), fallible_list: crate::agentic::agents_children());
