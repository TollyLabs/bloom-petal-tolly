// `GET /v1/routed`, filtered locally by the term in the file name.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "query") {
        Ok(query) => crate::agentic::routed_read(Some(query), crate::host::now_ms()),
        Err(response) => response,
    }
);
