// The same 50 rows as markets.json, as `<address>.json` files so `ls` works.
// Any token is still addressable by its lowercase address.
fn children() -> Result<Vec<petal::RouteChild>, petal::DispatchResponse> {
    let network = crate::api::Network::current()?;
    let list = crate::api::fetch_json(network, &crate::api::ApiRoute::Markets).map_err(|error| error.response())?;
    Ok(crate::api::market_addresses(&list)
        .into_iter()
        .map(|address| petal::file(format!("{address}.json")))
        .collect())
}

petal::route_file!(spec: petal::http_dir_spec(), fallible_list: children());
