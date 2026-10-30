// `GET /v1/exchange/stats` + `GET /v1/agents`: the agent exchange at a glance,
// how paying works (and why not from Bloom), and where to read next.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |_ctx: &petal::Ctx| crate::agentic::overview_read(crate::host::now_ms())
);
