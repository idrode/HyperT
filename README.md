# hyperT (nombre pendiente)

TUI de análisis para Hyperliquid DEX escrita en Rust. **Fase 1: solo lectura** —
Info API + WebSocket, sin wallet, sin firma, sin órdenes. Pensada para swing/position
trading (holds de días). Ver `CLAUDE.md` para el plan completo.

## Ejecutar

```sh
cargo run --release            # mainnet (solo lectura)
cargo run --release -- --testnet
```

Validación rápida de la capa de datos sin TUI:

```sh
cargo run --bin probe
```

## Vistas

1. **Ranking cross-pair** — todos los perps con precio en vivo (WS), 24h%, funding
   horario y APR, premium perp/oracle, OI notional, ΔOI 5m/1h y clasificación de
   flujo (entran longs / entran shorts / cierran shorts / cierran longs).
2. **Par** — detalle por par: velas sólidas (TF 1m/5m/15m/1h/4h/12h/1d con `i`), sub-panel
   RSI/ADX/DMI alineado bajo las velas, historial de funding (~3d), sparklines de OI y mid
   en vivo. RSI/ADX solo como confirmación secundaria.
3. **B+RSI** — panel Ballenas + RSI/ADX/DMI (port del Pine `whales+RSI`): RSI clásico + MA,
   RSI Modificado (%B), ADX/±DI, y disparos ▲▼ de compra/venta ballena con intensidad,
   checklist en vivo de condiciones y log de últimos disparos. TA puro sobre precio, estimado.
4. **Heatmap** — top pares por OI, color por funding APR, ΔOI 1h o cambio 24h (`m` cambia).
5. **Liqs** — mapa de niveles de liquidación por par (estimado; rango `r` ±5/15/30%).
6. **Flujo** — rotación de capital cross-pair (ΔOI notional en ventanas 1h/4h/24h, `w`
   cambia; convicción vs. acumulación silenciosa según volumen) y panel de posicionamiento
   del par fijado (`Enter`): percentil del funding contra su histórico de ~30d, premium
   sostenido, skew long/short de whales, asimetría de liquidaciones ±3% (estimada) y CVD
   en vivo del canal `trades`, con score compuesto de sobreextensión ▼/▲. Solo lectura.
7. **Whales** — posiciones de las cuentas top del leaderboard (`clearinghouseState` por dirección).
8. **Fondos** — Fase 2: conexión de la cuenta maestra MetaMask vía WalletConnect v2 (QR
   en terminal, `c` conecta / `d` desconecta). Depósito/retiro/agent wallet pendientes.
9. **Wallet** — seguimiento watch-only de cualquier dirección pública (posiciones, PnL, margen).

## Teclas

`1…9` o `Tab` vistas · `↑↓/jk` mover · `Enter` abrir/fijar par · `s` ordenar · `r` invertir ·
`←→/hl` par anterior/siguiente · `i` temporalidad · `m` métrica heatmap · `w` ventana flujo ·
`?` ayuda · `q` salir

## Notas de datos

- `metaAndAssetCtxs` se sondea cada 5 s (funding, OI, premium, oracle) — el ΔOI se
  calcula sobre esa historia en memoria, así que las ventanas 5m/1h necesitan que la
  app lleve abierta ~ese tiempo. La Vista 6 (Flujo) usa además un historial lento (1 muestra/min,
  ~25h): sus ventanas 1h/4h/24h muestran "—" hasta acumular uptime — es esperado.
- Los mids llegan por WebSocket (`allMids`) en tiempo real.
- Velas y funding history (~30d, paginado) se cargan bajo demanda al fijar un par;
  el CVD se suscribe al canal `trades` solo del par seleccionado.
