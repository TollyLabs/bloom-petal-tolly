// `GET /v1/acp` + `GET /v1/spend-policy`: is Tolly OS live (ACP escrow,
// agent spending rules), with addresses, fees and limits when it is.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |_ctx: &petal::Ctx| crate::agentic::os_read(crate::host::now_ms())
);
