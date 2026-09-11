// `GET {api}/token/<address>` projected: identity, provenance, venues with
// day-1 execution support, and the quote paths.
petal::route_file!(
    spec: petal::http_read_spec(5_000),
    read: |ctx: &petal::Ctx| {
        let address = match petal::param(ctx, "address").and_then(|value| {
            crate::amount::parse_route_address(value).map_err(|error| crate::err(-3, error))
        }) {
            Ok(address) => address,
            Err(response) => return response,
        };
        let network = crate::api::Network::current();
        match crate::api::token_detail(network, address) {
            Ok(detail) => petal::read_json_value(&crate::api::token_document(&detail)),
            Err(response) => response,
        }
    }
);
