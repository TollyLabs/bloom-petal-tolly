// One `<jb_ id>.json` per open job on the board.
petal::route_file!(spec: petal::http_dir_spec(), fallible_list: crate::agentic::jobs_children());
