mod app;
mod data;
mod exec;
mod flow;
mod i18n;
mod liq;
mod liqdens;
mod search;
mod signals;
mod trader;
mod tui;
mod ui;
mod wallet;

use anyhow::Result;
use hyperliquid_rust_sdk::BaseUrl;
use tokio::sync::{mpsc, watch};

use app::App;
use data::types::ExtraReq;
use tui::Tui;

#[tokio::main]
async fn main() -> Result<()> {
    // proveedor criptográfico de rustls para el WS de WalletConnect: sin esto
    // panica al negociar TLS porque el árbol no trae un proveedor por defecto
    let _ = rustls::crypto::ring::default_provider().install_default();

    // idioma de la interfaz: inglés por defecto (publicación en GitHub),
    // `--lang=es`/`--lang=en` o la tecla `L` para alternar en vivo.
    for a in std::env::args() {
        if let Some(v) = a.strip_prefix("--lang=") {
            if let Some(l) = i18n::parse_lang(v) {
                i18n::set_lang(l);
            }
        }
    }

    let testnet = std::env::args().any(|a| a == "--testnet");
    let (base, net_label) = if testnet {
        (BaseUrl::Testnet, "testnet")
    } else {
        (BaseUrl::Mainnet, "mainnet")
    };

    // si algo revienta, dejar la terminal usable antes de imprimir el pánico
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
        default_hook(info);
    }));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (extra_tx, extra_rx) = mpsc::channel::<ExtraReq>(16);
    let (wallet_tx, wallet_rx) = watch::channel(Vec::new());
    let (usdc_tx, usdc_rx) = watch::channel(None);
    let (coin_tx, coin_rx) = watch::channel(None);
    let (wc_tx, wc_rx) = mpsc::unbounded_channel();
    data::spawn_data_tasks(base, tx.clone(), extra_rx, wallet_rx, usdc_rx, coin_rx);
    // Vista 8 (Fondos): depósitos van por Arbitrum One en mainnet y Sepolia en testnet
    let chain_id = if testnet { 421_614 } else { 42_161 };
    wallet::walletconnect::spawn(tx.clone(), wc_rx, chain_id);

    // detección de protocolo gráfico/tamaño de celda (paneles de indicadores):
    // interroga al tty, así que debe ir ANTES del raw mode de Tui::new
    let gfx = ui::oscimg::Gfx::new();
    let mut app = App::new(extra_tx, wallet_tx, usdc_tx, coin_tx, wc_tx, net_label, gfx);

    // Panel de ejecución REAL (pasos 7 y 7.5): dos rutas EXPLÍCITAS y
    // separadas, nunca detección implícita — testnet se arma solo con
    // secrets/agent_testnet.json y mainnet solo con secrets/agent_mainnet.json
    // (el loader además verifica el hyperliquid_chain del archivo). Sin el
    // archivo de su red, el panel queda en maqueta. En mainnet la confirmación
    // exige escribir CONFIRMO (dinero real).
    let agent = if testnet {
        wallet::agent::load("Testnet")
    } else {
        wallet::agent::load("Mainnet")
    };
    if let Some(agent) = agent {
        let (trade_tx, trade_rx) = mpsc::unbounded_channel();
        data::spawn_orders_watcher(base, tx.clone(), agent.master);
        app.arm_trading(agent.master, agent.address.clone(), trade_tx);
        trader::spawn(base, tx.clone(), trade_rx, agent);
    }
    let mut tui = Tui::new()?;
    let res = tui.run(&mut app, &mut rx).await;
    drop(tui);
    res
}
