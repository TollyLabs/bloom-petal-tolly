// `GET {api}/health` projected, plus what this build is (network, chain,
// constants digest, whether the owner enabled writes).
petal::route_file!(
    spec: petal::http_read_spec(5_000),
    read: |_ctx: &petal::Ctx| {
        let network = crate::api::Network::current();
        let health = match crate::api::fetch_json(network, &crate::api::ApiRoute::Health) {
            Ok(health) => health,
            Err(error) => return error.response(),
        };
        petal::read_json_value(&crate::api::status_document(
            network,
            &health,
            crate::policy::writes_enabled(),
            crate::host::now_ms(),
        ))
    }
);
