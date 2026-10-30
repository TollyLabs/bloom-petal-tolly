// Searches are addressed, not listed: routed/search/<term>.json matches the
// term (case-insensitive) in each listing's title, description, seller,
// agent id and resource URL.
petal::route_file!(spec: petal::static_dir_spec(), list: Vec::new());
