// `GET /v1/services` projected: every TOLLY service with its price (atomic
// USDC and decimal), latest-version flag, availability and detail path.
petal::route_file!(
    spec: petal::http_read_spec(60_000),
    read: |_ctx: &petal::Ctx| crate::agentic::services_read(crate::host::now_ms())
);
