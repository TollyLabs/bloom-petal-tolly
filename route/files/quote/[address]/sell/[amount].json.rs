// Best-execution SELL quote (token -> USDC, no interface fee). Outputs are
// normalised to 6-decimal USDC before ranking; a native-quote V4 venue's raw
// 18-decimal output is reported alongside. Pure read, 2 s cache (M8).
petal::route_file!(
    spec: petal::http_read_spec(2_000).caps(&["bloom:http", "bloom:chain"]),
    read: |ctx: &petal::Ctx| {
        let address = match petal::param(ctx, "address").and_then(|value| {
            crate::amount::parse_route_address(value).map_err(|error| crate::err(-3, error))
        }) {
            Ok(address) => address,
            Err(response) => return response,
        };
        let amount_text = match petal::param(ctx, "amount") {
            Ok(value) => value,
            Err(response) => return response,
        };
        let network = crate::api::Network::current();
        let detail = match crate::api::token_detail(network, address) {
            Ok(detail) => detail,
            Err(response) => return response,
        };
        let amount = match crate::amount::parse_decimal(amount_text, detail.decimals) {
            Ok(amount) => amount,
            Err(error) => return crate::err(-3, format!("amount: {error}")),
        };
        let quote = crate::quote::quote(&detail, crate::quote::Side::Sell, amount, crate::policy::SLIPPAGE_DEFAULT_BPS);
        petal::read_json_value(&crate::quote::quote_document(&quote))
    }
);
