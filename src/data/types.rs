use std::time::Instant;

/// Metadatos estáticos de un perp (del universo de `meta`).
#[derive(Debug, Clone)]
pub struct PairMeta {
    pub name: String,
    pub sz_decimals: u32,
    pub max_leverage: usize,
}

/// Snapshot de contexto de un perp (de `metaAndAssetCtxs`).
#[derive(Debug, Clone, Copy)]
pub struct CtxSnapshot {
    pub t: Instant,
    /// Hora de pared del poll en ms epoch, para alinear OI a cierres de vela.
    pub t_ms: u64,
    pub mark_px: f64,
    pub mid_px: Option<f64>,
    pub oracle_px: f64,
    /// Funding rate horario en decimal (0.0000125 = 0.00125%/h).
    pub funding: f64,
    /// Open interest en unidades base del activo.
    pub open_interest: f64,
    pub premium: Option<f64>,
    pub day_ntl_vlm: f64,
    pub prev_day_px: f64,
}

/// Snapshot de OI histórico del servidor de backfill (`GET /oi`). Solo se
/// queda con lo que el TUI necesita para reconstruir ΔOI: momento, OI en
/// unidades base y mark. El notional se recalcula con el mark, como en vivo.
#[derive(Debug, Clone, Copy)]
pub struct BackfillOi {
    /// Hora de pared del snapshot, ms epoch.
    pub ts_ms: u64,
    pub oi: f64,
    pub mark_px: f64,
}

/// Bucket de 1 minuto de volumen agresor del servidor de backfill
/// (`GET /delta`), mismo esquema que los buckets en memoria de `DeltaState`.
#[derive(Debug, Clone, Copy)]
pub struct BackfillDelta {
    pub minute_ms: u64,
    pub buy_vol: f64,
    pub sell_vol: f64,
}

/// Temporalidad de velas de la vista de par / liquidaciones.
/// Los 7 valores están verificados contra `candleSnapshot` de la API real
/// (2026-07-16): todos devuelven velas con el string de `api()` tal cual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interval {
    M1,
    M5,
    M15,
    H1,
    H4,
    H12,
    D1,
}

impl Interval {
    pub const ALL: [Interval; 7] = [
        Interval::M1,
        Interval::M5,
        Interval::M15,
        Interval::H1,
        Interval::H4,
        Interval::H12,
        Interval::D1,
    ];

    pub fn api(&self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
            Interval::H12 => "12h",
            Interval::D1 => "1d",
        }
    }

    pub fn label(&self) -> &'static str {
        self.api()
    }

    pub fn ms(&self) -> u64 {
        match self {
            Interval::M1 => 60_000,
            Interval::M5 => 5 * 60_000,
            Interval::M15 => 15 * 60_000,
            Interval::H1 => 3_600_000,
            Interval::H4 => 4 * 3_600_000,
            Interval::H12 => 12 * 3_600_000,
            Interval::D1 => 24 * 3_600_000,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Interval::M1 => Interval::M5,
            Interval::M5 => Interval::M15,
            Interval::M15 => Interval::H1,
            Interval::H1 => Interval::H4,
            Interval::H4 => Interval::H12,
            Interval::H12 => Interval::D1,
            Interval::D1 => Interval::M1,
        }
    }
}

/// Petición de datos bajo demanda para la vista de par.
#[derive(Debug, Clone)]
pub struct ExtraReq {
    pub coin: String,
    pub interval: Interval,
}

/// Espejo de la vela de la API.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandlePoint {
    pub t_close: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Volumen en unidades base.
    pub volume: f64,
}

/// Posición abierta de una cuenta (de `clearinghouseState`).
#[derive(Debug, Clone)]
pub struct PosInfo {
    pub coin: String,
    /// Tamaño con signo en unidades base: >0 long, <0 short.
    pub szi: f64,
    pub entry_px: Option<f64>,
    /// Notional absoluto en USD.
    pub position_value: f64,
    pub unrealized_pnl: f64,
    pub roe: f64,
    pub leverage: u32,
    pub is_cross: bool,
    pub liq_px: Option<f64>,
    /// Funding acumulado (USD) desde la apertura (`cumFunding.sinceOpen`).
    /// Convención VERIFICADA empíricamente (2026-07-23): POSITIVO = el trader
    /// HA PAGADO funding (coste), NEGATIVO = lo ha COBRADO. Comprobado en una
    /// cuenta de una sola posición donde sinceOpen coincide al céntimo con
    /// −Σ(userFunding.usdc) (y usdc>0 = la cuenta recibe). Docs/SDK no lo
    /// documentan.
    pub since_open_funding: f64,
}

/// Cómo se ha averiguado la apertura del tramo actual de una posición, de más
/// a menos firme. La UI lo refleja para no vender una estimación como un dato
/// exacto (ver `src/data/opens.rs` para el detalle de cada vía).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenKind {
    /// Fill con cruce por cero (o flip de lado) localizado en el historial:
    /// dato exacto al segundo.
    Exact,
    /// Reconstruida acumulando `userFunding` hacia atrás hasta agotar el
    /// `cumFunding.sinceOpen` de la posición: precisión del evento de funding
    /// (horas), no del fill.
    Funding,
    /// No se ha podido determinar: es solo una cota inferior (la posición se
    /// abrió ESE instante o antes).
    LowerBound,
}

/// Apertura estimada del tramo actual de una posición abierta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenEst {
    pub ms: u64,
    pub kind: OpenKind,
}

/// Un fill del historial de operaciones de una cuenta (`userFills`). Solo
/// lectura de datos públicos, para la Vista 9 (win rate, PnL realizado,
/// operaciones cerradas). La API devuelve una lista plana, más reciente
/// primero, con tope de ~2000 fills (verificado 2026-07-23) — suficiente
/// para el resumen sin paginar con `userFillsByTime`.
#[derive(Debug, Clone)]
pub struct FillInfo {
    pub coin: String,
    /// `dir` tal cual lo reporta la API: "Buy", "Sell", "Close Long",
    /// "Open Short", "Long > Short"… — se muestra, no se reinterpreta.
    pub dir: String,
    /// Precio y tamaño (unidades base) del fill.
    pub px: f64,
    pub sz: f64,
    /// Tamaño con signo de la posición ANTES de este fill (`startPosition`):
    /// clave para localizar la apertura de la posición actual (cuando cruzó 0).
    pub start_position: f64,
    /// PnL realizado por ESTE fill. Solo los fills de cierre lo traen ≠ 0.
    pub closed_pnl: f64,
    /// Comisión pagada (USD equivalente aproximado; el token real es feeToken).
    pub fee: f64,
    /// Timestamp de ejecución en ms.
    pub time_ms: u64,
}

/// Una transferencia NO de trading entre la cuenta observada y OTRA dirección
/// (`userNonFundingLedgerUpdates`), para las listas de wallets relacionadas de
/// la Vista 9. Solo se conservan los movimientos que tienen contraparte
/// identificable: los depósitos/retiros del bridge, las liquidaciones y los
/// traspasos spot⇄perps no la tienen y se descartan (ver `parse_transfer`).
#[derive(Debug, Clone)]
pub struct TransferInfo {
    /// La OTRA dirección (nunca la observada), en el formato de la API.
    pub counterparty: String,
    /// true = la observada RECIBIÓ fondos de `counterparty`.
    pub incoming: bool,
    /// Tipo crudo del delta ("internalTransfer", "spotTransfer", "send",
    /// "subAccountTransfer", "vaultDeposit"…) — se muestra, no se reinterpreta.
    pub kind: String,
    /// Token movido ("USDC" para los traspasos de perps).
    pub token: String,
    /// Cantidad en unidades del token.
    pub amount: f64,
    /// Valor en USD si la API lo reporta (`usdcValue`); para USDC = amount.
    pub usd: Option<f64>,
    pub time_ms: u64,
}

/// Cuenta grande trackeada desde el leaderboard.
#[derive(Debug, Clone)]
pub struct WhaleInfo {
    pub addr: String,
    pub account_value: f64,
    pub positions: Vec<PosInfo>,
}

/// Estado de una cuenta observada (wallet watch-only). Solo lectura de datos públicos.
#[derive(Debug, Clone)]
pub struct AccountSnapshot {
    pub addr: String,
    pub account_value: f64,
    pub withdrawable: f64,
    pub total_margin_used: f64,
    pub total_ntl_pos: f64,
    pub positions: Vec<PosInfo>,
}

/// Resumen del saldo SPOT dentro de Hyperliquid (spotClearinghouseState) de
/// la cuenta maestra — deliberadamente separado del de perps
/// (`AccountSnapshot`): son dos saldos distintos. El faucet de testnet y las
/// compras spot acreditan AQUÍ, no en perps (verificado 2026-07-20: los 999
/// USDC del faucet estaban en spot mientras el TUI enseñaba 0 de perps).
#[derive(Debug, Clone)]
pub struct SpotSnapshot {
    pub addr: String,
    /// USDC total en spot; `hold` es lo retenido en órdenes abiertas — lo
    /// transferible a perps es `total - hold`.
    pub usdc_total: f64,
    pub usdc_hold: f64,
    /// USDC disponible tras margen de mantenimiento
    /// (`tokenToAvailableAfterMaintenance`, token 0). En cuenta unificada es
    /// EL margen operable/retirable real; None si la API no lo reporta.
    pub usdc_avail: Option<f64>,
    /// Otros tokens spot con saldo > 0, (coin, total) — solo para mostrar.
    pub others: Vec<(String, f64)>,
}

/// Orden abierta REAL de la cuenta de trading (de `frontendOpenOrders`, que
/// a diferencia de `openOrders` trae tipo de orden y datos de trigger —
/// forma verificada contra la API real el 2026-07-20).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveOrd {
    pub coin: String,
    /// side "B" = compra.
    pub is_buy: bool,
    /// `orderType` tal cual lo reporta la API ("Limit", "Stop Market",
    /// "Take Profit Market", …) — se muestra, no se interpreta más allá
    /// de detectar SL/TP por prefijo.
    pub kind: String,
    /// Precio efectivo: triggerPx si es trigger, limitPx si no.
    pub px: f64,
    pub sz: f64,
    pub oid: u64,
    pub reduce_only: bool,
    pub is_trigger: bool,
}

impl LiveOrd {
    /// ¿Es un trigger de cierre (SL o TP) — reduce-only y con disparo?
    pub fn is_close_trigger(&self) -> bool {
        self.is_trigger && self.reduce_only
    }

    pub fn is_sl(&self) -> bool {
        self.kind.starts_with("Stop")
    }

    pub fn is_tp(&self) -> bool {
        self.kind.starts_with("Take Profit")
    }
}

/// Modo de cuenta de Hyperliquid (`userAbstraction` en /info). Verificado en
/// vivo el 2026-07-20: la cuenta real responde "unifiedAccount" (mainnet Y
/// testnet); direcciones sin el modo nuevo responden "default".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountMode {
    /// Spot y perps comparten un único fondo de margen: `usdClassTransfer`
    /// está deshabilitado y `clearinghouseState` NO es significativo — la
    /// fuente de verdad del saldo/margen es `spotClearinghouseState`.
    Unified,
    /// Cualquier otro valor ("default", "portfolioMargin", …): se conserva
    /// el modelo clásico de dos saldos separados con transferencia interna.
    Standard(String),
}

/// Mensajes de la capa de datos hacia el TUI.
#[derive(Debug, Clone)]
pub enum DataMsg {
    /// Refresco completo de universo + contextos (REST, cada pocos segundos).
    Ctxs(Vec<(PairMeta, CtxSnapshot)>),
    /// Mids en vivo del WebSocket (solo perps). OJO: el servidor empuja
    /// allMids solo cada ~5s (verificado 2026-07-10, SDK y WS crudo).
    Mids(Vec<(String, f64)>),
    /// Mid del BBO del par seleccionado (suscripción por-coin, sub-segundo).
    CoinMid {
        coin: String,
        mid: f64,
    },
    /// Datos bajo demanda para la vista de par: velas + historial de funding.
    PairExtra {
        coin: String,
        interval: Interval,
        candles: Vec<CandlePoint>,
        /// (timestamp ms, funding horario decimal)
        funding_hist: Vec<(u64, f64)>,
    },
    /// Notional agregado de un batch del canal `trades` del par seleccionado,
    /// separado por lado agresor (B compra / A venta) — CVD de la Vista 6 y
    /// delta por vela de la Vista 2. `t_ms` = tiempo del último trade del batch
    /// (para bucketizar el delta por vela; el CVD lo ignora).
    CoinTrades {
        coin: String,
        buy_ntl: f64,
        sell_ntl: f64,
        t_ms: u64,
    },
    /// Posiciones de las cuentas top del leaderboard.
    Whales(Vec<WhaleInfo>),
    /// Progreso/estado del tracker de whales (el leaderboard pesa ~30MB).
    WhaleStatus(String),
    /// Estado de la dirección observada en la vista wallet.
    WalletState(AccountSnapshot),
    /// Historial de fills (`userFills`) de una dirección observada (Vista 9).
    WalletFills {
        addr: String,
        fills: Vec<FillInfo>,
    },
    /// Aperturas reconstruidas de las posiciones abiertas de una dirección
    /// observada (Vista 9), por par. Llega tarde y a su ritmo: reconstruirlas
    /// cuesta decenas de peticiones (ver `opens::resolve`).
    WalletOpens {
        addr: String,
        opens: std::collections::HashMap<String, OpenEst>,
    },
    /// Transferencias con contraparte (`userNonFundingLedgerUpdates`) de una
    /// dirección observada — wallets relacionadas de la Vista 9.
    WalletTransfers {
        addr: String,
        transfers: Vec<TransferInfo>,
    },
    /// Saldo SPOT dentro de Hyperliquid de la cuenta maestra WC (Vista 8).
    SpotState(SpotSnapshot),
    /// Modo de cuenta (`userAbstraction`) de la maestra WC — decide si el
    /// margen operable sale de spot (unificada) o de perps (estándar).
    AccountMode {
        addr: String,
        mode: AccountMode,
    },
    /// Órdenes abiertas REALES de la cuenta de trading (frontendOpenOrders).
    OpenOrders {
        addr: String,
        orders: Vec<LiveOrd>,
    },
    /// Fase/resultado de una acción real del panel de ejecución (trader).
    Exec(crate::trader::ExecEvent),
    /// Saldo USDC on-chain (wallet en Arbitrum) de la cuenta maestra WC —
    /// Pieza 1 del depósito, eth_call de solo lectura. `usdc: None` = la
    /// chain de la sesión no tiene RPC/contrato USDC mapeado.
    UsdcBalance {
        /// Mismo formato canónico que `AccountSnapshot.addr`.
        addr: String,
        usdc: Option<f64>,
    },
    /// Estado de la conexión WalletConnect (Vista 8, Fondos).
    Wc(crate::wallet::walletconnect::WcStatus),
    /// Fase del depósito real al bridge (Pieza 2): lo emiten el gestor WC
    /// (firma) y el vigilante del receipt (confirmación on-chain).
    Deposit(crate::wallet::walletconnect::DepositStatus),
    /// Fase del retiro real (paso 5): firma EIP-712 → aceptado por
    /// Hyperliquid → USDC llegado a la wallet (o fallo con motivo).
    Withdraw(crate::wallet::walletconnect::WithdrawStatus),
    /// Fase de la autorización de agent wallet (paso 6): firma EIP-712 →
    /// aceptada por Hyperliquid + clave guardada → verificada en extraAgents.
    Agent(crate::wallet::walletconnect::AgentStatus),
    /// Fase de la transferencia interna spot⇄perps (usdClassTransfer):
    /// firma EIP-712 → aceptada → reflejada en el saldo destino (o fallo).
    Transfer(crate::wallet::walletconnect::TransferStatus),
    /// Historial traído de un servidor de backfill externo (opcional, ver
    /// `data::backfill`): snapshots de OI y buckets de delta de 1 min de un
    /// par, para sembrar el historial en memoria en vez de arrancar vacío.
    Backfill {
        coin: String,
        oi: Vec<BackfillOi>,
        delta: Vec<BackfillDelta>,
    },
    WsStatus(bool),
    RestError(String),
}
