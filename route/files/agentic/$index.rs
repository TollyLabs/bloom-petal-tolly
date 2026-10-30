// TOLLY's agent exchange and Tolly OS (agent.tollylabs.com), read only.
// guide.md at the Petal root maps every file here.
petal::route_file!(
    spec: petal::static_dir_spec(),
    list: {
        let mut children = petal::files(&[
            "overview.json",
            "services.json",
            "routed.json",
            "agents.json",
            "jobs.json",
            "os.json",
        ]);
        children.extend(petal::dir_names(&["services", "routed", "agents", "jobs", "acp", "spending"]));
        children
    }
);
