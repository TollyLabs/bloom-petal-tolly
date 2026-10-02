//! The wallet and account selected by Bloom for a scoped Tolly invocation.
use std::cell::Cell;
thread_local! {
    static ACCOUNT: Cell<u32> = const { Cell::new(0) };
}

pub fn wallet_param(ctx: &petal::Ctx) -> Result<&str, petal::DispatchResponse> {
    let wallet = petal::route_param(ctx, "bloom.wallet")
        .ok_or_else(|| petal::error(-2, "Bloom did not select a wallet for Tolly"))?;
    crate::wallet::check_wallet_id(wallet)?;
    let number = petal::route_param(ctx, "bloom.account")
        .ok_or_else(|| petal::error(-2, "Bloom did not select an account for Tolly"))?
        .parse::<u32>()
        .map_err(|_| petal::error(-3, "invalid selected account"))?;
    ACCOUNT.with(|account| account.set(number));
    Ok(wallet)
}

pub fn number() -> u32 {
    ACCOUNT.with(Cell::get)
}

pub fn link(wallet: &str, path: &str) -> String {
    format!("wallets/{wallet}/{}/{path}", number())
}
#[cfg(test)]
pub(crate) fn with_selected_for_test<T>(account: u32, _prefix: &str, run: impl FnOnce() -> T) -> T {
    struct Restore(u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACCOUNT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(ACCOUNT.with(|n| n.replace(account)));
    run()
}
