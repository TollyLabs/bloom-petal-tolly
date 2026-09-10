petal::route_file!(
    spec: petal::static_dir_spec(),
    list: {
        let mut children = vec![
            petal::writable("buy.json"),
            petal::writable("sell.json"),
            petal::writable("launch.json"),
            petal::file("positions.json"),
        ];
        children.push(petal::dir("operations"));
        children
    }
);
