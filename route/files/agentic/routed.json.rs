// `GET /v1/routed` projected: other agents' x402 services TOLLY resells
// (all-in price, seller price, fee, refund rule), plus paused sellers.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |_ctx: &petal::Ctx| crate::agentic::routed_read(None, crate::host::now_ms())
);
