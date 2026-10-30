// `GET /v1/acp/jobs?limit=50`: the newest Tolly ACP escrow jobs, or the
// component's off state (not deployed, disabled, not on this gateway yet).
petal::route_file!(
    spec: petal::http_read_spec(10_000),
    read: |_ctx: &petal::Ctx| crate::agentic::acp_jobs_read(crate::host::now_ms())
);
