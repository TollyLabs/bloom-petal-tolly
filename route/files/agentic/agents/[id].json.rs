// `GET /v1/agents/<id>`: one agent's profile, services and links.
petal::route_file!(
    spec: petal::http_read_spec(30_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "id") {
        Ok(id) => crate::agentic::agent_read(id, crate::host::now_ms()),
        Err(response) => response,
    }
);
