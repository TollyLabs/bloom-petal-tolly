// `GET /v1/nft-agents/<uuid>/policy`: an agent wallet's on-chain spending
// rules (AgentSpendPolicy), or the component's off state. Read only.
petal::route_file!(
    spec: petal::http_read_spec(15_000),
    read: |ctx: &petal::Ctx| match petal::param(ctx, "agent") {
        Ok(agent) => crate::agentic::spending_read(agent, crate::host::now_ms()),
        Err(response) => response,
    }
);
