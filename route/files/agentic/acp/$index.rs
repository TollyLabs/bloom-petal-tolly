petal::route_file!(
    spec: petal::static_dir_spec(),
    list: {
        let mut children = petal::files(&["jobs.json"]);
        children.extend(petal::dir_names(&["jobs"]));
        children
    }
);
