//! Imprime un JWT válido del relay para probar el endpoint HTTP con curl.
use walletconnect_sdk::relay_auth::RelayAuth;
use walletconnect_sdk::utils::random_bytes32;

fn main() {
    let auth = RelayAuth::new(random_bytes32());
    println!("{}", auth.sign_jwt("https://relay.walletconnect.org"));
}
