// `GET /v1/acp/jobs/<jobId>`: one escrow job, who may act next, its event
// timeline and whether the stored delivery matches the on-chain hash.
petal::route_file!(
    spec: petal::http_read_spec(10_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "id") {
        Ok(id) => crate::agentic::acp_job_read(id, crate::host::now_ms()),
        Err(response) => response,
    }
);
