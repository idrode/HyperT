//! Panel de ejecución de futuros (Vista 8, Fondos) — estado del formulario,
//! filas de posiciones/órdenes y funciones puras de conversión/validación.
//!
//! Dos modos, decididos por `ExecState::real` (paso 7 de Fase 2):
//! - MAQUETA (default, y siempre en mainnet por ahora): "enviar" solo muta
//!   las listas demo en memoria; nada toca la Exchange API.
//! - REAL (solo testnet, con agent key autorizada presente): las filas se
//!   sincronizan desde la cuenta de verdad (app::sync_exec_rows) y las
//!   acciones viajan al Exchange API vía crate::trader — las mutaciones
//!   locales de este módulo (place/close_pos/cancel_ord/apply_sltp) NO se
//!   usan en ese modo.

use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn flip(self) -> Self {
        match self {
            Side::Long => Side::Short,
            Side::Short => Side::Long,
        }
    }

    pub fn is_long(self) -> bool {
        self == Side::Long
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::Long => "LONG",
            Side::Short => "SHORT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdType {
    Market,
    Limit,
}

impl OrdType {
    pub fn flip(self) -> Self {
        match self {
            OrdType::Market => OrdType::Limit,
            OrdType::Limit => OrdType::Market,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OrdType::Market => crate::i18n::t().ex_market,
            OrdType::Limit => crate::i18n::t().ex_limit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
    Usd,
    Asset,
}

impl SizeUnit {
    pub fn flip(self) -> Self {
        match self {
            SizeUnit::Usd => SizeUnit::Asset,
            SizeUnit::Asset => SizeUnit::Usd,
        }
    }
}

/// Campo/fila enfocable del panel. Una única cadena lineal recorrible con
/// ↑↓ (formulario → posiciones → órdenes → vuelta al formulario).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Pair,
    Side,
    Lev,
    OrdType,
    LimitPx,
    Size,
    Sl,
    Tp,
    Submit,
    Pos(usize),
    Ord(usize),
}

/// Cadena de foco vigente (LimitPx solo existe con orden límite).
pub fn chain(typ: OrdType, n_pos: usize, n_ord: usize) -> Vec<Focus> {
    let mut v = vec![Focus::Pair, Focus::Side, Focus::Lev, Focus::OrdType];
    if typ == OrdType::Limit {
        v.push(Focus::LimitPx);
    }
    v.extend([Focus::Size, Focus::Sl, Focus::Tp, Focus::Submit]);
    v.extend((0..n_pos).map(Focus::Pos));
    v.extend((0..n_ord).map(Focus::Ord));
    v
}

/// Avanza `delta` pasos en la cadena de foco, con vuelta circular.
pub fn step_focus(cur: Focus, delta: i64, typ: OrdType, n_pos: usize, n_ord: usize) -> Focus {
    let ch = chain(typ, n_pos, n_ord);
    let i = ch.iter().position(|f| *f == cur).unwrap_or(0) as i64;
    ch[(i + delta).rem_euclid(ch.len() as i64) as usize]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdKind {
    Limit,
    Sl,
    Tp,
}

impl OrdKind {
    pub fn label(self) -> &'static str {
        match self {
            OrdKind::Limit => crate::i18n::t().ex_limit,
            OrdKind::Sl => crate::i18n::t().ex_stop,
            OrdKind::Tp => "TP",
        }
    }
}

/// Fila de posición del panel: simulada (maqueta) o REAL (sincronizada del
/// clearinghouseState en modo real). Mismo criterio de signo que `PosInfo::szi`.
#[derive(Debug, Clone)]
pub struct MockPos {
    pub coin: String,
    pub szi: f64,
    pub entry: f64,
    pub lev: u32,
    pub sl: Option<f64>,
    pub tp: Option<f64>,
    /// Precio de liquidación REAL reportado por la API (modo real); en
    /// maqueta va None y la UI enseña la estimación de `liq_price`.
    pub liq: Option<f64>,
    /// Sembrada como demo al arrancar (vs. creada en esta sesión).
    pub demo: bool,
}

impl MockPos {
    pub fn is_long(&self) -> bool {
        self.szi >= 0.0
    }
}

/// Fila de orden abierta del panel (límite de entrada o trigger SL/TP) —
/// simulada, o REAL si trae `oid` (necesario para cancelarla de verdad).
#[derive(Debug, Clone)]
pub struct MockOrd {
    pub coin: String,
    pub side: Side,
    pub kind: OrdKind,
    pub px: f64,
    /// Tamaño en unidades base.
    pub sz: f64,
    /// oid real de Hyperliquid (modo real); None en maqueta.
    pub oid: Option<u64>,
    pub demo: bool,
}

/// Borrador validado de una orden, listo para el resumen de confirmación.
#[derive(Debug, Clone)]
pub struct OrderDraft {
    pub coin: String,
    pub side: Side,
    pub lev: u32,
    pub typ: OrdType,
    pub entry: f64,
    pub sz_usd: f64,
    pub sz_asset: f64,
    pub liq: Option<f64>,
    pub sl: Option<f64>,
    pub tp: Option<f64>,
}

/// Frase de confirmación reforzada de mainnet (dinero real, paso 7.5).
pub const MAINNET_PHRASE: &str = "CONFIRMO";

#[derive(Debug, Clone)]
pub enum Confirm {
    Order(OrderDraft),
    Close(usize),
}

/// Modal de edición de SL/TP de una posición ya abierta.
#[derive(Debug, Clone)]
pub struct SlTpEdit {
    pub pos: usize,
    pub sl: String,
    pub tp: String,
    pub on_tp: bool,
    pub err: Option<String>,
}

/// Objetivos clicables; el draw reconstruye los rects en cada frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Focus(Focus),
    /// Click en un input de texto: foco + entra en edición.
    Edit(Focus),
    SetSide(Side),
    SetType(OrdType),
    SetUnit(SizeUnit),
    PairStep(i64),
    LevStep(i64),
    LevSlider,
    Submit,
    ConfirmYes,
    ConfirmNo,
    ClosePos,
    EditSlTp,
    CancelOrd,
    /// Alternar el sentido en el modal de transferencia spot⇄perps.
    XferDir,
}

pub struct ExecState {
    pub focus: Focus,
    /// Edición de texto en curso del campo enfocado (LimitPx/Size/Sl/Tp).
    pub editing: bool,
    pub side: Side,
    pub lev: u32,
    /// Buffer de edición numérica del apalancamiento (Enter sobre el campo).
    pub lev_edit: Option<String>,
    pub typ: OrdType,
    pub limit_px: String,
    pub size: String,
    pub unit: SizeUnit,
    pub sl: String,
    pub tp: String,
    pub confirm: Option<Confirm>,
    /// Fricción extra SOLO mainnet real (paso 7.5): buffer de la frase que el
    /// usuario debe teclear ("CONFIRMO") antes de que Enter ejecute. `None` =
    /// confirmación normal y/n (maqueta o testnet).
    pub confirm_phrase: Option<String>,
    pub sltp: Option<SlTpEdit>,
    pub positions: Vec<MockPos>,
    pub orders: Vec<MockOrd>,
    pub seeded: bool,
    pub status: Option<String>,
    pub err: Option<String>,
    /// (rect, objetivo) de los elementos clicables del último frame.
    pub hits: Vec<(Rect, Hit)>,
    /// Arrastre del slider de apalancamiento en curso.
    pub lev_drag: bool,
    /// Modo REAL (paso 7): posiciones/órdenes vienen de la cuenta de verdad
    /// y las acciones van al Exchange API vía el trader — nada de maqueta.
    pub real: bool,
}

impl ExecState {
    pub fn new() -> Self {
        Self {
            focus: Focus::Pair,
            editing: false,
            side: Side::Long,
            lev: 5,
            lev_edit: None,
            typ: OrdType::Market,
            limit_px: String::new(),
            size: "100".to_string(),
            unit: SizeUnit::Usd,
            sl: String::new(),
            tp: String::new(),
            confirm: None,
            confirm_phrase: None,
            sltp: None,
            positions: Vec::new(),
            orders: Vec::new(),
            seeded: false,
            status: None,
            err: None,
            hits: Vec::new(),
            lev_drag: false,
            real: false,
        }
    }

    /// El panel captura el teclado (nada debe llegar a atajos globales).
    pub fn captures(&self) -> bool {
        self.editing || self.lev_edit.is_some() || self.confirm.is_some() || self.sltp.is_some()
    }

    /// Input de texto del campo enfocado, si el foco es un campo de texto.
    pub fn focused_input_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::LimitPx => Some(&mut self.limit_px),
            Focus::Size => Some(&mut self.size),
            Focus::Sl => Some(&mut self.sl),
            Focus::Tp => Some(&mut self.tp),
            _ => None,
        }
    }

    /// Si el foco apunta a una fila que ya no existe, lo repliega al botón.
    pub fn clamp_focus(&mut self) {
        match self.focus {
            Focus::Pos(i) if i >= self.positions.len() => {
                self.focus = match self.positions.len() {
                    0 => Focus::Submit,
                    n => Focus::Pos(n - 1),
                };
            }
            Focus::Ord(i) if i >= self.orders.len() => {
                self.focus = match self.orders.len() {
                    0 => Focus::Submit,
                    n => Focus::Ord(n - 1),
                };
            }
            Focus::LimitPx if self.typ == OrdType::Market => self.focus = Focus::OrdType,
            _ => {}
        }
    }

    /// "Ejecuta" el borrador confirmado contra las listas demo.
    pub fn place(&mut self, d: &OrderDraft) {
        let close_side = d.side.flip();
        match d.typ {
            OrdType::Market => {
                let szi = if d.side.is_long() {
                    d.sz_asset
                } else {
                    -d.sz_asset
                };
                self.positions.push(MockPos {
                    coin: d.coin.clone(),
                    szi,
                    entry: d.entry,
                    lev: d.lev,
                    sl: d.sl,
                    tp: d.tp,
                    liq: None,
                    demo: false,
                });
                self.status = Some(
                    crate::i18n::t()
                        .ex_st_mkt_sim
                        .replacen("{}", d.side.label(), 1)
                        .replacen("{}", &d.coin, 1),
                );
            }
            OrdType::Limit => {
                self.orders.push(MockOrd {
                    coin: d.coin.clone(),
                    side: d.side,
                    kind: OrdKind::Limit,
                    px: d.entry,
                    sz: d.sz_asset,
                    oid: None,
                    demo: false,
                });
                self.status = Some(
                    crate::i18n::t()
                        .ex_st_lim_sim
                        .replacen("{}", d.side.label(), 1)
                        .replacen("{}", &d.coin, 1),
                );
            }
        }
        for (kind, px) in [(OrdKind::Sl, d.sl), (OrdKind::Tp, d.tp)] {
            if let Some(px) = px {
                self.orders.push(MockOrd {
                    coin: d.coin.clone(),
                    side: close_side,
                    kind,
                    px,
                    sz: d.sz_asset,
                    oid: None,
                    demo: false,
                });
            }
        }
        self.err = None;
    }

    /// Cierra a mercado una posición demo y retira SOLO sus triggers SL/TP
    /// (otra posición del mismo par conserva los suyos).
    pub fn close_pos(&mut self, i: usize) {
        if i >= self.positions.len() {
            return;
        }
        let p = self.positions.remove(i);
        retain_other_triggers(&mut self.orders, &p);
        self.status = Some(crate::i18n::t().ex_st_pos_closed.replacen("{}", &p.coin, 1));
        self.clamp_focus();
    }

    pub fn cancel_ord(&mut self, i: usize) {
        if i >= self.orders.len() {
            return;
        }
        let o = self.orders.remove(i);
        self.status = Some(
            crate::i18n::t()
                .ex_st_ord_cancelled
                .replacen("{}", o.kind.label(), 1)
                .replacen("{}", &o.coin, 1),
        );
        self.clamp_focus();
    }

    /// Aplica SL/TP nuevos a una posición y sincroniza sus triggers.
    pub fn apply_sltp(&mut self, i: usize, sl: Option<f64>, tp: Option<f64>) {
        let Some(p) = self.positions.get_mut(i) else {
            return;
        };
        p.sl = sl;
        p.tp = tp;
        let (coin, sz, close_side) = (p.coin.clone(), p.szi.abs(), {
            if p.is_long() {
                Side::Short
            } else {
                Side::Long
            }
        });
        let p = p.clone();
        retain_other_triggers(&mut self.orders, &p);
        for (kind, px) in [(OrdKind::Sl, sl), (OrdKind::Tp, tp)] {
            if let Some(px) = px {
                self.orders.push(MockOrd {
                    coin: coin.clone(),
                    side: close_side,
                    kind,
                    px,
                    sz,
                    oid: None,
                    demo: false,
                });
            }
        }
        self.status = Some(crate::i18n::t().ex_st_sltp_updated.replacen("{}", &coin, 1));
    }

    /// Valida el formulario y construye el borrador para el resumen.
    pub fn draft(&self, coin: &str, mid: f64, max_lev: usize) -> Result<OrderDraft, String> {
        let entry = match self.typ {
            OrdType::Market => {
                if mid <= 0.0 {
                    return Err(crate::i18n::t().ex_err_no_price.into());
                }
                mid
            }
            OrdType::Limit => parse_num(&self.limit_px).ok_or("precio límite inválido")?,
        };
        let v = parse_num(&self.size).ok_or("tamaño inválido")?;
        let (sz_usd, sz_asset) = size_both(v, self.unit, entry).ok_or("sin precio del par")?;
        if sz_usd < 10.0 {
            return Err(crate::i18n::t().ex_err_min_ntl.into());
        }
        let long = self.side.is_long();
        let sl = parse_trigger(&self.sl, entry, long, true).map_err(|e| format!("SL: {e}"))?;
        if let Some(px) = sl {
            if let Some(m) = trigger_side_err(px, entry, long, true) {
                return Err(m.into());
            }
        }
        let tp = parse_trigger(&self.tp, entry, long, false).map_err(|e| format!("TP: {e}"))?;
        if let Some(px) = tp {
            if let Some(m) = trigger_side_err(px, entry, long, false) {
                return Err(m.into());
            }
        }
        Ok(OrderDraft {
            coin: coin.to_string(),
            side: self.side,
            lev: self.lev,
            typ: self.typ,
            entry,
            sz_usd,
            sz_asset,
            liq: liq_price(entry, self.lev, max_lev, long),
            sl,
            tp,
        })
    }

    /// Puebla las listas demo una única vez, con precios reales como ancla
    /// para que el PnL en vivo de la maqueta se mueva de verdad. En modo
    /// REAL no siembra jamás: las filas son la cuenta de verdad.
    pub fn seed(&mut self, btc: Option<f64>, eth: Option<f64>, sol: Option<f64>) {
        if self.seeded || self.real {
            return;
        }
        let (Some(b), Some(e)) = (btc, eth) else {
            return;
        };
        if b <= 0.0 || e <= 0.0 {
            return;
        }
        self.seeded = true;
        self.positions.push(MockPos {
            coin: "BTC".into(),
            szi: 0.05,
            entry: b * 0.985,
            lev: 10,
            sl: Some(b * 0.94),
            tp: Some(b * 1.06),
            liq: None,
            demo: true,
        });
        self.positions.push(MockPos {
            coin: "ETH".into(),
            szi: -1.5,
            entry: e * 1.01,
            lev: 5,
            sl: Some(e * 1.05),
            tp: Some(e * 0.92),
            liq: None,
            demo: true,
        });
        for p in self.positions.clone() {
            let close_side = if p.is_long() { Side::Short } else { Side::Long };
            for (kind, px) in [(OrdKind::Sl, p.sl), (OrdKind::Tp, p.tp)] {
                if let Some(px) = px {
                    self.orders.push(MockOrd {
                        coin: p.coin.clone(),
                        side: close_side,
                        kind,
                        px,
                        sz: p.szi.abs(),
                        oid: None,
                        demo: true,
                    });
                }
            }
        }
        if let Some(s) = sol.filter(|s| *s > 0.0) {
            self.orders.push(MockOrd {
                coin: "SOL".into(),
                side: Side::Long,
                kind: OrdKind::Limit,
                px: s * 0.95,
                sz: 10.0,
                oid: None,
                demo: true,
            });
        }
    }
}

/// Deja en `orders` todo menos los triggers SL/TP que pertenecen a `p`
/// (mismo par, lado de cierre y tamaño): identificarlos por par a secas
/// arrasaría los triggers de otra posición del mismo par.
fn retain_other_triggers(orders: &mut Vec<MockOrd>, p: &MockPos) {
    let close_side = if p.is_long() { Side::Short } else { Side::Long };
    let sz = p.szi.abs();
    orders.retain(|o| {
        !(o.coin == p.coin
            && matches!(o.kind, OrdKind::Sl | OrdKind::Tp)
            && o.side == close_side
            && (o.sz - sz).abs() < 1e-12)
    });
}

/// Número positivo simple ("1000", "108250.5").
pub fn parse_num(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// (USD, unidades base) del tamaño según la unidad en la que se escribió.
pub fn size_both(v: f64, unit: SizeUnit, px: f64) -> Option<(f64, f64)> {
    if px <= 0.0 || v <= 0.0 {
        return None;
    }
    Some(match unit {
        SizeUnit::Usd => (v, v / px),
        SizeUnit::Asset => (v * px, v),
    })
}

/// SL/TP: precio directo ("117000") o distancia porcentual ("2%") aplicada
/// desde `entry` hacia el lado natural del campo (SL contra la posición, TP
/// a favor). Vacío = sin trigger.
pub fn parse_trigger(s: &str, entry: f64, long: bool, is_sl: bool) -> Result<Option<f64>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Some(p) = s.strip_suffix('%') {
        let pct: f64 = p
            .trim()
            .parse()
            .map_err(|_| crate::i18n::t().ex_err_pct_invalid.replacen("{}", s, 1))?;
        if !pct.is_finite() || pct <= 0.0 {
            return Err(crate::i18n::t().ex_err_pct_positive.into());
        }
        if entry <= 0.0 {
            return Err(crate::i18n::t().ex_err_no_entry_pct.into());
        }
        let down = is_sl == long;
        let px = entry
            * if down {
                1.0 - pct / 100.0
            } else {
                1.0 + pct / 100.0
            };
        if px <= 0.0 {
            return Err(crate::i18n::t().ex_err_dist_100.into());
        }
        return Ok(Some(px));
    }
    match parse_num(s) {
        Some(px) => Ok(Some(px)),
        None => Err(crate::i18n::t().ex_err_px_invalid.replacen("{}", s, 1)),
    }
}

/// None si el trigger cae en el lado correcto de la entrada; mensaje si no.
pub fn trigger_side_err(px: f64, entry: f64, long: bool, is_sl: bool) -> Option<&'static str> {
    let below = px < entry;
    if below == (is_sl == long) {
        return None;
    }
    let tr = crate::i18n::t();
    Some(match (long, is_sl) {
        (true, true) => tr.ex_err_sl_long,
        (true, false) => tr.ex_err_tp_long,
        (false, true) => tr.ex_err_sl_short,
        (false, false) => tr.ex_err_tp_short,
    })
}

/// Precio de liquidación estimado con margen aislado y mantenimiento
/// mmr = 1/(2·max_lev) — la mitad del margen inicial a leverage máximo, como
/// documenta Hyperliquid. Estimación de maqueta: la real depende del modo
/// cross y del margen libre de la cuenta.
pub fn liq_price(entry: f64, lev: u32, max_lev: usize, long: bool) -> Option<f64> {
    if entry <= 0.0 || lev == 0 || max_lev == 0 {
        return None;
    }
    let l = lev as f64;
    let mmr = 1.0 / (2.0 * max_lev as f64);
    let px = if long {
        entry * (1.0 - 1.0 / l) / (1.0 - mmr)
    } else {
        entry * (1.0 + 1.0 / l) / (1.0 + mmr)
    };
    (px > 0.0).then_some(px)
}

/// uPnL en USD y ROE% de una posición mock al mark actual.
pub fn pos_pnl(p: &MockPos, mark: f64) -> Option<(f64, f64)> {
    if mark <= 0.0 || p.entry <= 0.0 {
        return None;
    }
    let pnl = p.szi * (mark - p.entry);
    let margin = p.szi.abs() * p.entry / p.lev.max(1) as f64;
    (margin > 0.0).then(|| (pnl, pnl / margin * 100.0))
}

/// Columna relativa dentro del slider → leverage (1..=max), lineal por celda.
pub fn slider_lev(x_off: u16, width: u16, max: u32) -> u32 {
    if width <= 1 || max <= 1 {
        return max.max(1);
    }
    let frac = x_off.min(width - 1) as f64 / (width - 1) as f64;
    (1.0 + frac * (max as f64 - 1.0)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_omits_limit_px_on_market() {
        assert!(!chain(OrdType::Market, 0, 0).contains(&Focus::LimitPx));
        assert!(chain(OrdType::Limit, 0, 0).contains(&Focus::LimitPx));
    }

    #[test]
    fn step_focus_wraps_both_ways() {
        let last = Focus::Ord(1);
        assert_eq!(step_focus(last, 1, OrdType::Market, 1, 2), Focus::Pair);
        assert_eq!(step_focus(Focus::Pair, -1, OrdType::Market, 1, 2), last);
    }

    #[test]
    fn step_focus_walks_rows() {
        assert_eq!(
            step_focus(Focus::Submit, 1, OrdType::Market, 2, 1),
            Focus::Pos(0)
        );
        assert_eq!(
            step_focus(Focus::Pos(1), 1, OrdType::Market, 2, 1),
            Focus::Ord(0)
        );
    }

    #[test]
    fn liq_long_below_entry_and_tighter_with_less_lev() {
        let e = 100_000.0;
        let l10 = liq_price(e, 10, 40, true).unwrap();
        let l20 = liq_price(e, 20, 40, true).unwrap();
        assert!(l10 < e && l20 < e);
        // más leverage → liquidación más cerca de la entrada
        assert!(l20 > l10);
        // long 1× no tiene precio de liquidación alcanzable
        assert!(liq_price(e, 1, 40, true).is_none());
    }

    #[test]
    fn liq_short_above_entry() {
        let e = 100_000.0;
        let s = liq_price(e, 10, 40, false).unwrap();
        assert!(s > e);
        // short 1× sí se liquida (el precio puede subir sin límite)
        assert!(liq_price(e, 1, 40, false).unwrap() > e);
    }

    #[test]
    fn trigger_pct_goes_to_natural_side() {
        // SL de un long: 2% por debajo; TP: 2% por encima
        let sl = parse_trigger("2%", 100.0, true, true).unwrap().unwrap();
        let tp = parse_trigger("2%", 100.0, true, false).unwrap().unwrap();
        assert!((sl - 98.0).abs() < 1e-9 && (tp - 102.0).abs() < 1e-9);
        // en un short se invierte
        let sl_s = parse_trigger("2%", 100.0, false, true).unwrap().unwrap();
        assert!((sl_s - 102.0).abs() < 1e-9);
        assert!(parse_trigger("", 100.0, true, true).unwrap().is_none());
        assert!(parse_trigger("abc", 100.0, true, true).is_err());
    }

    #[test]
    fn trigger_side_validation() {
        assert!(trigger_side_err(98.0, 100.0, true, true).is_none());
        assert!(trigger_side_err(102.0, 100.0, true, true).is_some());
        assert!(trigger_side_err(102.0, 100.0, false, true).is_none());
        assert!(trigger_side_err(98.0, 100.0, false, false).is_none());
    }

    #[test]
    fn size_conversion_both_units() {
        let (usd, asset) = size_both(1000.0, SizeUnit::Usd, 100_000.0).unwrap();
        assert!((usd - 1000.0).abs() < 1e-9 && (asset - 0.01).abs() < 1e-12);
        let (usd2, asset2) = size_both(0.01, SizeUnit::Asset, 100_000.0).unwrap();
        assert!((usd2 - 1000.0).abs() < 1e-9 && (asset2 - 0.01).abs() < 1e-12);
        assert!(size_both(1.0, SizeUnit::Usd, 0.0).is_none());
    }

    #[test]
    fn slider_maps_edges_and_middle() {
        assert_eq!(slider_lev(0, 16, 40), 1);
        assert_eq!(slider_lev(15, 16, 40), 40);
        assert_eq!(slider_lev(100, 16, 40), 40);
        let mid = slider_lev(8, 16, 40);
        assert!((15..=25).contains(&mid));
    }

    #[test]
    fn draft_validates_and_market_place_creates_pos_and_triggers() {
        let mut st = ExecState::new();
        st.size = "1000".into();
        st.sl = "2%".into();
        st.tp = "5%".into();
        let d = st.draft("BTC", 100_000.0, 40).unwrap();
        assert!((d.sz_asset - 0.01).abs() < 1e-12);
        assert!(d.liq.unwrap() < 100_000.0);
        st.place(&d);
        assert_eq!(st.positions.len(), 1);
        // SL y TP quedan como órdenes trigger abiertas
        assert_eq!(st.orders.len(), 2);
        // otra posición del MISMO par con sus propios triggers
        st.positions.push(MockPos {
            coin: "BTC".into(),
            szi: 0.05,
            entry: 99_000.0,
            lev: 10,
            sl: Some(95_000.0),
            tp: None,
            liq: None,
            demo: false,
        });
        st.apply_sltp(1, Some(95_000.0), None);
        assert_eq!(st.orders.len(), 3);
        st.close_pos(0);
        assert!(st.positions.len() == 1);
        // cerrar retira SOLO los triggers de la posición cerrada
        assert_eq!(st.orders.len(), 1);
        assert!((st.orders[0].sz - 0.05).abs() < 1e-12);
    }

    #[test]
    fn draft_rejects_wrong_side_sl_and_dust() {
        let mut st = ExecState::new();
        st.size = "1000".into();
        st.sl = "105000".into(); // SL por encima de la entrada en un long
        assert!(st.draft("BTC", 100_000.0, 40).is_err());
        st.sl.clear();
        st.size = "5".into(); // < $10 notional
        assert!(st.draft("BTC", 100_000.0, 40).is_err());
    }

    #[test]
    fn limit_draft_uses_limit_price() {
        let mut st = ExecState::new();
        st.typ = OrdType::Limit;
        st.limit_px = "90000".into();
        st.size = "900".into();
        let d = st.draft("BTC", 100_000.0, 40).unwrap();
        assert!((d.entry - 90_000.0).abs() < 1e-9);
        assert!((d.sz_asset - 0.01).abs() < 1e-12);
        st.place(&d);
        assert!(st.positions.is_empty());
        assert_eq!(st.orders.len(), 1);
    }

    #[test]
    fn apply_sltp_syncs_trigger_orders() {
        let mut st = ExecState::new();
        st.positions.push(MockPos {
            coin: "BTC".into(),
            szi: 0.05,
            entry: 100_000.0,
            lev: 10,
            sl: None,
            tp: None,
            liq: None,
            demo: false,
        });
        st.apply_sltp(0, Some(95_000.0), Some(110_000.0));
        assert_eq!(st.orders.len(), 2);
        st.apply_sltp(0, Some(96_000.0), None);
        assert_eq!(st.orders.len(), 1);
        assert_eq!(st.positions[0].sl, Some(96_000.0));
    }
}
