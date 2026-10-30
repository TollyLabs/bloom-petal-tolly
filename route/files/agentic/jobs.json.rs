// `GET /v1/jobs?status=open&limit=50` projected: the open job board.
petal::route_file!(
    spec: petal::http_read_spec(15_000),
    read: |_ctx: &petal::Ctx| crate::agentic::jobs_read(crate::host::now_ms())
);
