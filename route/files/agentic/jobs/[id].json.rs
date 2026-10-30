// `GET /v1/jobs/<jb_ id>`: one board job and its delivery previews (results
// stay hidden until the poster unlocks one).
petal::route_file!(
    spec: petal::http_read_spec(10_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "id") {
        Ok(id) => crate::agentic::job_read(id, crate::host::now_ms()),
        Err(response) => response,
    }
);
