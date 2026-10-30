// Spending rules are addressed by agent NFT id: spending/<uuid>.json (the
// id is in agentic/agents.json `spending_rules`, or /r/agents/<uuid>).
petal::route_file!(spec: petal::static_dir_spec(), list: Vec::new());
