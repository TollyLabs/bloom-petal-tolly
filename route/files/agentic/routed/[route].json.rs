// `GET /v1/routed/<routeId>`: one routed listing.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "route") {
        Ok(route) => crate::agentic::routed_one_read(route, crate::host::now_ms()),
        Err(response) => response,
    }
);
