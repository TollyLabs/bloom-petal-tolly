// `GET /v1/agents` projected: the listed agents with measured revenue,
// sales, checks, token and profile paths.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |_ctx: &petal::Ctx| crate::agentic::agents_read(crate::host::now_ms())
);
