// ACP jobs are addressed by number: acp/jobs/<jobId>.json (ids are in
// acp/jobs.json).
petal::route_file!(spec: petal::static_dir_spec(), list: Vec::new());
