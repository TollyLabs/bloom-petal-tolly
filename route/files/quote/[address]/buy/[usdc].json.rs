// Best-execution BUY quote: API detail -> venues -> eth_call simulations at
// gross minus the interface fee (external tokens only), ranked, with the
// protected minimum output for the day-1 executable winner. Pure eth_calls
// at the latest block; uncached (critique M8).
petal::route_file!(
    spec: petal::chain_read_spec().caps(&["bloom:http", "bloom:chain"]),
    read: |ctx: &petal::Ctx| {
        let address = match petal::param(ctx, "address").and_then(|value| {
            crate::amount::parse_route_address(value).map_err(|error| crate::err(-3, error))
        }) {
            Ok(address) => address,
            Err(response) => return response,
        };
        let amount = match petal::param(ctx, "usdc").and_then(|value| {
            crate::amount::parse_decimal(value, crate::constants::USDC_ERC20_DECIMALS)
                .map_err(|error| crate::err(-3, format!("usdc: {error}")))
        }) {
            Ok(amount) => amount,
            Err(response) => return response,
        };
        let network = match crate::api::Network::current() {
            Ok(network) => network,
            Err(response) => return response,
        };
        let detail = match crate::api::token_detail(network, address) {
            Ok(detail) => detail,
            Err(response) => return response,
        };
        let quote = crate::quote::quote(&detail, crate::quote::Side::Buy, amount, crate::policy::SLIPPAGE_DEFAULT_BPS);
        petal::read_json_value(&crate::quote::quote_document(&quote))
    }
);
