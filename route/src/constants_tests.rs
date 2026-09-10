//! Drift checks for the generated `constants.rs` (kept out of the generated
//! file so the generator can overwrite it freely).

use alloy_primitives::Address;
use sha2::{Digest, Sha256};

use crate::constants::*;

const VENDORED: &[u8] = include_bytes!("../../chain/arc.testnet.json");

fn addr(value: &serde_json::Value) -> Address {
    value
        .as_str()
        .expect("address string")
        .parse()
        .expect("address")
}

#[test]
fn vendored_source_matches_the_digest_and_the_constants() {
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(VENDORED)));
    assert_eq!(digest, SOURCE_DIGEST, "re-run scripts/gen-constants.mjs");
    let source: serde_json::Value = serde_json::from_slice(VENDORED).unwrap();
    assert_eq!(source["CHAIN_ID"].as_u64(), Some(CHAIN_ID));
    assert_eq!(addr(&source["USDC"]), USDC);
    assert_eq!(addr(&source["PAD"]), PAD);
    assert_eq!(addr(&source["FURNACE"]), FURNACE);
    assert_eq!(addr(&source["MULTI_ROUTER"]), MULTI_ROUTER);
    assert_eq!(addr(&source["TOLLY_SWAP_ROUTER"]), TOLLY_SWAP_ROUTER);
    assert_eq!(addr(&source["V4_ROUTER"]), V4_ROUTER);
    assert_eq!(addr(&source["LOCKER"]), LOCKER);
    assert_eq!(addr(&source["TOLLY"]), TOLLY);
    assert_eq!(
        source["V4_ROUTER_CODEHASH"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(V4_ROUTER_CODEHASH)
    );
    assert_eq!(source["api"].as_str(), Some(API_PROD));
    assert_eq!(CHAIN, "arc");
    assert_eq!(INTERFACE_FEE_BPS, 20);
    assert_eq!(POOL_FEE_PAD, 10_000);
    assert_eq!(PAD_TOTAL_SUPPLY_RAW.len(), 28, "1e27 has 28 digits");
}

#[test]
fn shared_infrastructure_addresses_are_the_known_arc_ones() {
    // From src/data/chains.ts INFRA[ARC], venues.ts and v4Execution.ts.
    assert_eq!(
        format!("{SWAP_ROUTER02:?}"),
        "0x53bf6b0684ec7ef91e1387da3d1a1769bc5a6f77"
    );
    assert_eq!(
        format!("{V3_FACTORY:?}"),
        "0xf0db7b58379503491d857db50ac9ece64c653918"
    );
    assert_eq!(
        format!("{POSITION_MANAGER:?}"),
        "0x39654a85a4c05127f5fd6ed22caec077a0fb1377"
    );
    assert_eq!(
        format!("{QUOTER_V2:?}"),
        "0x7dfd4f31be6814d2906bde155c3e1b146eac1468"
    );
    assert_eq!(
        format!("{V4_QUOTER:?}"),
        "0x8dc178efb8111bb0973dd9d722ebeff267c98f94"
    );
    assert_eq!(
        format!("{UNIVERSAL_ROUTER:?}"),
        "0x4fca4a51ab4f23a7447b3284fbd7d73289a89fb1"
    );
    assert_eq!(
        format!("{PERMIT2:?}"),
        "0x000000000022d473030f116ddee9f6b43ac78ba3"
    );
    assert_eq!(V4_NATIVE_CURRENCY, Address::ZERO);
}

/// Inside the monorepo the served overlay must equal the vendored copy; after
/// extraction the file is absent and the check is skipped.
#[test]
fn monorepo_overlay_matches_the_vendored_copy_when_present() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../public/testnet.json");
    if let Ok(bytes) = std::fs::read(&path) {
        assert_eq!(
            bytes, VENDORED,
            "public/testnet.json changed: re-run scripts/gen-constants.mjs"
        );
    }
}
