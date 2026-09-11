// `GET {api}/tokens?scope=ours&sort=volume&dir=desc&limit=50` projected (D3).
petal::route_file!(
    spec: petal::http_read_spec(10_000),
    read: |_ctx: &petal::Ctx| {
        let network = crate::api::Network::current();
        let list = match crate::api::fetch_json(network, &crate::api::ApiRoute::Markets) {
            Ok(list) => list,
            Err(error) => return error.response(),
        };
        match crate::api::markets_document(&list, crate::host::now_ms()) {
            Ok(document) => petal::read_json_value(&document),
            Err(error) => crate::err(-4, format!("TOLLY API: {error}")),
        }
    }
);
