// `GET /v1/services/<id>`: price, input/output schema, example, limits,
// limitations, refund policy and how to buy it (never from this Petal).
petal::route_file!(
    spec: petal::http_read_spec(60_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "id") {
        Ok(id) => crate::agentic::service_read(id, crate::host::now_ms()),
        Err(response) => response,
    }
);
