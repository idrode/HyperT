# Hyperliquid Trading TUI (nombre pendiente)

## Qué es esto
Interfaz de trading en terminal (TUI) para Hyperliquid DEX, escrita en Rust.
Pensada para swing/position trading (holds de días), NO para scalping automático de alta frecuencia.

## Estado actual del proyecto: arrancando FASE 2 — fondos reales y ejecución

### 🚨 DOS HALLAZGOS CRÍTICOS (verificados contra código el 2026-08-05, resolver YA)
1. **El repositorio NO tiene ni un solo commit.** Todo el proyecto (~19.600 líneas, incluido código
   que mueve fondos reales) existe solo como working tree — un accidente de disco lo borra entero.
   Verificar `.gitignore` cubre `/secrets/` (`git check-ignore -v secrets/agent_mainnet.json`) y
   hacer el primer commit YA, antes de cualquier otra tarea. Prioridad absoluta.
2. **`secrets/agent_testnet.json` NO EXISTE** — solo está `agent_mainnet.json`. Esto bloquea el
   paso 8 (E2E en testnet) e invierte la red de seguridad: hoy la ÚNICA red donde el panel de
   ejecución puede armarse es MAINNET, con fondos reales. Autorizar la agent wallet de testnet
   (tecla `a` con `--testnet` y el móvil) es la siguiente acción real más urgente tras el commit.

Fase 1 funcionalmente completa (ranking, vista de par, heatmap, whale positioning, wallet watch-only,
heatmap de liquidaciones estimado, panel Ballenas+RSI/ADX/DMI). Bug de latencia de precio resuelto
(ver nota histórica más abajo si reaparece). A partir de aquí, TODO lo que sigue toca fondos reales
y firma — máxima cautela, sin `--dangerously-skip-permissions` sin supervisión activa en esta fase.

## Reordenamiento de vistas — ✅ APLICADO
Confirmado en la cabecera del TUI: 1 Ranking, 2 Par, 3 Ballenas+RSI/ADX/DMI, 4 Heatmap top-OI,
5 Heatmap liquidaciones, 6 Flujo de Dinero, 7 Whale positioning, 8 Fondos, 9 Wallet watch-only.
Mapeo original (para referencia histórica, ya no accionable):
El orden de teclas 1-9 va a cambiar para reflejar un flujo de uso más lógico. Mapeo definitivo,
número VIEJO → número NUEVO (actualizar la tecla de acceso Y toda referencia interna/UI, no solo
el código — también renombrar en comentarios/docs donde aparezca "Vista N" si aplica):

| Nombre                              | Tecla vieja | Tecla nueva |
|--------------------------------------|:-----------:|:-----------:|
| Ranking cross-pair                   | 1           | 1 (sin cambio) |
| Par (velas + sub-panel)              | 2           | 2 (sin cambio) |
| Ballenas + RSI/ADX/DMI               | 7           | **3**       |
| Heatmap top-OI                       | 3           | **4**       |
| Heatmap de liquidaciones (estimado)  | 6           | **5**       |
| Flujo de Dinero / Posicionamiento    | 9           | **6**       |
| Whale positioning (leaderboard)      | 4           | **7**       |
| Fondos (WalletConnect)               | 8           | 8 (sin cambio) |
| Wallet en modo seguimiento           | 5           | **9**       |

A partir de este cambio, todas las menciones futuras en este documento a "Vista N" ya usan la
numeración NUEVA de la tabla — si hay inconsistencia en secciones de abajo escritas antes de este
cambio, la tabla de arriba manda.

## Pulido visual pendiente (aplicar el mismo tratamiento que ya funcionó en las velas de Vista 2)

### ⚠️ Spike previo: renderizado real vía protocolo gráfico Kitty (antes de comprometer el enfoque)
El usuario referenció https://github.com/vincelwt/gloomberb (finance terminal en TS/Bun) como
objetivo estético: usa OpenTUI sobre el protocolo de gráficos Kitty para dibujar líneas con
antialiasing real (píxeles), no caracteres — de ahí su aspecto pulido tipo TradingView. Esto es
replicable en Rust/ratatui, con dos candidatos a comparar en un spike aislado antes de decidir:
- **`ratatui-image`** (crate oficial del org ratatui) + `plotters` para rasterizar el oscilador
  como imagen real, mostrada vía Kitty/Sixel/iTerm2 con fallback automático a medios-bloques si el
  terminal no soporta gráficos. Es la ruta más fiel al estilo Gloomberb, pero solo se ve "de verdad
  pulida" en terminales con protocolo Kitty (Kitty, Ghostty, WezTerm) — verificar cuál usa el
  usuario y qué aspecto tiene el fallback en los demás.
- **`ratatui-plt`** (crate nuevo, v0.0.2, "matplotlib para terminal") con widgets nativos
  (`LinePlot`, `Heatmap`) que exportan a Kitty/Sixel/ANSI sin capa de rasterizado aparte — más
  simple de integrar, pero su propio README admite calidad inconsistente en varios widgets en esta
  versión temprana. Su `CandlestickPlot` en concreto tiene problemas conocidos, así que NO usar
  para las velas de Vista 2 (esas ya están resueltas con dibujo manual) — evaluar solo para
  `LinePlot` en los paneles de indicadores.

### ✅ Spike de renderizado Kitty — resuelto, ratatui-image + plotters gana
Veredicto con evidencia visual capturada (spike/chart_render/): `ratatui-image` + `plotters`
produce curvas suaves con antialiasing real (rasterizado 2x + reducción Lanczos), fallback
halfblocks legible en terminales sin protocolo gráfico. `ratatui-plt` DESCARTADO: su trazo sigue
siendo puntos Braille (mismo problema que ya se resolvió en las velas), y su "soporte Kitty" es
solo un export offline sin rasterizado real de glifos (verificado en su código fuente).

**Antes de implementar en producción — migración obligatoria primero, en sesión propia:**
`ratatui-image` requiere ratatui ^0.30.1; hyperT está en 0.29 y ya no hay versión de ratatui-image
para 0.29. Migrar 0.29→0.30 en una sesión AISLADA (solo la subida de versión + arreglo de breaking
changes + validación de que las 9 vistas existentes siguen funcionando igual, cero cambio visual),
con su propio commit, ANTES de tocar nada del renderizado nuevo. No mezclar ambos cambios en una
sola sesión — si algo falla, hay que poder aislar si fue la migración o el renderizado nuevo.

Notas técnicas para cuando se implemente el renderizado (después de la migración):
- `ratatui-image` con `default-features = false` (evitar arrastrar libchafa, librería C no instalada).
- Función compartida `oscilador → RgbImage` con plotters, reusada entre Vista 2 (sub-panel) y
  Vista 3 (Ballenas+RSI/ADX/DMI) — misma serie de datos, mismo renderizado.
- Coste de transmisión (~3MB por re-render) es irrelevante en local salvo que se quiera redibujar
  a cadencia sub-segundo — no es el caso aquí (redibuja por cambio de vela o resize).
- El hover horario ya planeado no se bloquea: la imagen ocupa celdas conocidas, mapeo columna→vela
  funciona igual que con el dibujo manual.
- Documentar en el README (para cuando se publique) el requisito de terminal Kitty-compatible para
  el mejor renderizado, con fallback automático a halfblocks en otros terminales.

Dos sitios siguen usando el Canvas con marcador Braille (líneas de puntos) para RSI/ADX/DMI, que
ya se demostró que se ve peor que un dibujo manual con caracteres de bloque sólido. Aplicar el
renderizado ganador del spike (ratatui-image + plotters, tras la migración a 0.30) en:

1. **Panel Ballenas + RSI/ADX/DMI (nueva Vista 3)**: sustituir las líneas Braille de RSI, MA, %B,
   ADX, +DI, -DI por un renderizado más sólido/visual (línea gruesa con caracteres de bloque, o
   relleno tipo área bajo la línea) — mismo criterio estético que las velas de Vista 2.
2. **Sub-panel RSI/ADX/DMI bajo las velas (Vista 2)**: mismo cambio — quitar el Canvas Braille,
   usar el mismo estilo de renderizado sólido que el punto 1 (idealmente compartir la misma función
   de dibujo entre ambos paneles, ya que son la misma serie de datos).

Además, en ambos sitios (Vista 2 y la nueva Vista 3) añadir **indicador horario al pasar el ratón**:
al hacer hover sobre cualquiera de los paneles de indicadores, mostrar en la parte baja del panel
la hora/timestamp de la vela/punto señalado (coherente con el hover de OHLC que ya existe en las
velas de Vista 2 — extender ese mismo mecanismo a los paneles de indicadores, no duplicar lógica).


### ⚠️ Primer paso obligatorio de Fase 2: spike de WalletConnect antes de construir nada más
Riesgo conocido: el ecosistema WalletConnect v2 en Rust está inmaduro. Candidato: crate
`walletconnect_client` (rol dApp — es el que necesitamos, ya que el TUI actúa como dApp y MetaMask
como wallet), que cubre iniciar sesión + generar URI/QR, pero su cobertura de firma de transacciones
post-conexión (eth_sendTransaction, eth_signTypedData_v4 para EIP-712) no está confirmada. Otra
alternativa (`nlordell/walletconnect-rs`) está marcada como WIP, solo handshake, sin firma.
ANTES de construir la pestaña de fondos: hacer un spike aislado que (1) genere el QR, (2) empareje
con MetaMask, (3) pida firmar un mensaje simple de prueba, (4) confirme que la firma vuelve
correctamente. Si esto no funciona con las librerías existentes en un tiempo razonable, el plan B
es implementar el protocolo WalletConnect v2 mínimo a mano (pairing vía relay + cifrado + JSON-RPC
sobre el relay) — más trabajo, pero controlado. Reportar el resultado del spike antes de seguir.

### Testnet primero, mainnet después
Hyperliquid tiene testnet. Toda la pestaña de fondos + ejecución de órdenes debe probarse contra
testnet (fondos de prueba, sin riesgo real) antes de validarla contra mainnet con fondos propios.
No saltarse este paso por prisa — es la única red de seguridad real en esta fase.

## Nueva pestaña: Fondos y Ejecución (Fase 2)

### Conexión de wallet
- Botón/tecla para "Conectar MetaMask" → genera URI WalletConnect v2, se muestra como QR en el TUI
  (reusar alguna librería de QR en terminal, ej. `qrcode` + render con caracteres Unicode/braille).
- MetaMask (móvil o extensión con soporte WC) escanea/aprueba el pairing.
- Sesión activa persiste mientras la app corre; reconexión manual si expira o se cierra la app.
- Esta conexión de MetaMask es la cuenta MAESTRA (ver Firma más abajo) — no se usa para cada orden,
  solo para: depósito, retiro, y autorización/rotación de la agent wallet.

### Depósito (USDC vía Arbitrum → Hyperliquid L1) — ⚠️ PRÓXIMA TAREA REAL (toca fondos, supervisión activa)
Contexto confirmado por el usuario: tiene ETH en Arbitrum en la wallet conectada, pendiente de
convertir/usar para obtener USDC. Quiere ver su saldo de USDC on-chain (Arbitrum) directamente en
el TUI, y poder depositarlo a Hyperliquid desde ahí — todo dentro de la Vista 8.

**Pieza 1 — saldo on-chain de USDC en Arbitrum (nueva, solo lectura, sin riesgo):**
- Leer el balance ERC20 de USDC (contrato de USDC en Arbitrum, verificar la dirección oficial
  correcta antes de asumirla — hay más de un "USDC" en Arbitrum, usar el nativo/oficial de Circle,
  no un puente de otro USDC) para la dirección de la sesión WalletConnect activa, vía una llamada
  `eth_call` de solo lectura a un RPC de Arbitrum (puede ser un RPC público, no requiere firma).
- Mostrar este saldo en la Vista 8, diferenciado claramente del saldo de Hyperliquid ya
  implementado — son dos cosas distintas: "USDC en tu wallet (disponible para depositar)" vs.
  "saldo dentro de Hyperliquid". No mezclarlos visualmente.

**Pieza 2 — flujo de depósito real (toca fondos, requiere firma real):**
- Flujo: approve ERC20 (USDC en Arbitrum) al contrato bridge de Hyperliquid → llamada de depósito
  al bridge. Ambas transacciones se firman vía la sesión WalletConnect activa (MetaMask aprueba en
  el móvil/extensión).
- Verificar dirección de destino visualmente antes de confirmar (mostrar en el TUI la dirección
  exacta a la que se deposita, para que el usuario la compare contra lo que ve en MetaMask).
- Confirmación explícita de cantidad antes de firmar — recordar que el primer depósito real debe
  ser pequeño ($5-10), no una cantidad grande, ya que es la primera transacción de fondos reales
  que sale de la app.
- Tras confirmar on-chain, el saldo debería reflejarse en Hyperliquid tras el puente (tiempo de
  confirmación de Arbitrum + procesamiento del bridge, no es instantáneo) — el saldo de Hyperliquid
  ya implementado (Vista 8) debería reflejarlo solo con esperar, sin necesitar más código.

Nota de secuencia recomendada: construir primero la Pieza 1 (solo lectura) en una sesión, verificar
que el saldo de USDC on-chain se lee bien, y solo después construir la Pieza 2 (transacciones
reales) en una sesión aparte con supervisión activa — mismo patrón de aislar riesgo que se ha usado
en el resto del proyecto (spike de WalletConnect, migración de ratatui, etc.).

### ⚠️ CORRECCIÓN CRÍTICA: Hyperliquid "Unified Account Mode" — invalida el supuesto de spot≠perps
Descubierto en producción (mainnet, cuenta real del usuario): al intentar la transferencia spot→perps
recién construida, Hyperliquid respondió "action disabled when unified account is active". Investigado
contra la documentación oficial de Hyperliquid: existe un modo de cuenta llamado **Unified Account**
(aparentemente el nuevo default para cuentas recientes) donde spot y perps **comparten un único fondo
de margen automáticamente** — no son carteras separadas. Bajo este modo:
- `usdClassTransfer` está deshabilitado a propósito (no tiene sentido transferir algo ya unificado).
- La propia doc de Hyperliquid dice textualmente que con cuenta unificada, los estados de perp dex
  individuales (`clearinghouseState`) NO SON SIGNIFICATIVOS — la fuente de verdad única para balance
  (spot Y disponible para perps) es `spotClearinghouseState`.
- Existe un endpoint `userAbstraction` (verificar en el SDK/API si está expuesto) que devuelve el modo
  de cuenta: `unifiedAccount`, `portfolioMargin`, o `disabled`/estándar (separado, el supuesto original
  con el que se construyó todo hasta ahora).

**Esto invalida parcialmente lo ya construido y hay que corregirlo ANTES de conectar el panel de
ejecución real (punto 7 del orden de Fase 2)** — si no, el cálculo de margen disponible para abrir
posiciones será incorrecto en cuentas unificadas (mostraría 0 disponible cuando en realidad el saldo
spot completo es utilizable).

**Qué corregir:**
1. Al conectar (o periódicamente), consultar el modo de cuenta (`userAbstraction` o equivalente) para
   la dirección de la sesión activa.
2. Si es **unificada**: usar `spotClearinghouseState` como fuente de verdad tanto para el bloque de
   saldo mostrado como para el cálculo de margen disponible del panel de ejecución. Ocultar o
   deshabilitar la tecla `t` (transferencia) con un mensaje claro ("no aplica: cuenta unificada"), en
   vez de dejar que el usuario reciba el error crudo de Hyperliquid.
3. Si es **estándar/no unificada**: mantener el comportamiento ya construido (dos saldos separados,
   transferencia `t` funcional) — este caso sigue siendo válido para cuentas que no usan el modo nuevo.
4. Revisar también el bloque de saldo de perps ya existente (Vista 8): bajo cuenta unificada, debería
   dejar de mostrarse como "0" de forma confusa — mostrar en su lugar que el modo es unificado y que
   el saldo relevante es el bloque spot de arriba.

Sesión recomendada: verificar primero contra testnet Y mainnet qué modo de cuenta tiene el usuario en
cada red (podrían diferir), implementar la detección y las dos ramas de comportamiento, y validar en
vivo contra la cuenta real del usuario (que ya sabemos está en modo unificado en mainnet).

### Transferencia interna Spot ↔ Perps — construida, ver corrección crítica arriba (solo aplica a
### cuentas NO unificadas)
Descubierto en producción: tras el depósito real (Pieza 2), el usuario vio sus 5 USDC en la web de
Hyperliquid pero el TUI mostraba "Valor cuenta 0". Diagnóstico confirmado con curl directo a la API
(sin pasar por el TUI): `clearinghouseState` (perps) devolvía accountValue 0.0, mientras que
`spotClearinghouseState` mostraba los 5 USDC completos. Causa raíz real (revisada): no es solo que
"haga falta transferir" — en cuentas unificadas ni siquiera hace falta, ver corrección arriba.

**Qué construir:** una transferencia interna Spot → Perps (y Perps → Spot, para poder revertir)
usando la acción `usdClassTransfer` del Exchange API de Hyperliquid — SOLO disponible/mostrada para
cuentas en modo estándar (no unificado, ver corrección arriba). Ya construida (tecla `t`, verificada
por hash EIP-712 de referencia contra el SDK de Python, sonda real contra `/exchange`), pendiente
ahora de condicionarla a que no se ofrezca en cuentas unificadas.
- Firmado con la cuenta maestra vía WalletConnect (no con la agent wallet — es movimiento de fondos
  entre carteras internas, no una orden de trading).
- Mostrar ambos saldos (spot y perps) SIEMPRE visibles y diferenciados en la Vista 8 — hasta ahora
  solo se mostraba el saldo de perps (`clearinghouseState`), lo que causó la confusión inicial.
  Añadir también el saldo spot (`spotClearinghouseState`) como su propio bloque, igual que ya se
  diferencia el saldo on-chain (USDC en wallet) del saldo dentro de Hyperliquid.
- Confirmación explícita antes de transferir, con resumen (cantidad, dirección spot→perps o
  viceversa) — mismo nivel de cautela que el resto de operaciones de fondos, aunque esta en
  concreto no tenga coste de gas ni sea irreversible (se puede revertir con otra transferencia).
- Considerar si tiene sentido ofrecer la opción de transferir automáticamente tras un depósito
  confirmado (con confirmación del usuario, no automático sin más), para que el flujo depósito→listo
  para operar quede autocontenido dentro del TUI sin tener que ir a la web de Hyperliquid para este
  paso intermedio.

### Retiro
- Solicitud de retiro firmada (EIP-712) vía WalletConnect → Hyperliquid procesa y envía a la
  dirección de origen en Arbitrum. Mostrar tiempos de espera típicos si Hyperliquid los documenta.

### Visualización de saldo y posiciones — ✅ HECHO (parcial — falta el saldo spot, ver arriba)
Reusa clearinghouseState (mismo watcher que Vista 9, ahora como lista de direcciones) aplicado
automáticamente a la dirección de la sesión WalletConnect activa. Ver detalle de implementación en
memoria del proyecto y commits recientes. Pendiente: añadir también spotClearinghouseState como
bloque separado (ver "Transferencia interna Spot ↔ Perps" arriba).

### Agent wallet (para operar sin fricción del día a día)
- Autorización de la agent wallet: firma EIP-712 única vía WalletConnect (cuenta maestra autoriza
  una clave nueva con permiso de trading, sin permiso de retiro).
- La agent key generada se guarda localmente (fuera de git, ver nota de seguridad abajo) y se usa
  como `LocalWallet` normal del SDK para firmar órdenes del día a día — sin pasar por WalletConnect
  en cada trade, coherente con la decisión ya tomada anteriormente.
- Rotación: repetir el proceso de autorización si se quiere invalidar la agent key actual.

### Panel de ejecución (long/short) — especificación completa tipo exchange
Objetivo: que se sienta como el panel de trading de cualquier exchange de futuros real (Binance,
Bybit, la propia web de Hyperliquid), solo que dentro del TUI, con interacción por teclado Y ratón.

**Entrada de orden:**
- Selección de par (reusar selector ya existente en otras vistas).
- Dirección: long o short — toggle claro, no ambiguo.
- Apalancamiento: input numérico, respetando el máximo permitido por par (ya se muestra "lev máx"
  en Vista 2, reusar ese dato). Considerar un slider/stepper clicable con el ratón además del input
  de teclado.
- Tamaño de posición: por USD notional o por cantidad del activo — dar ambas opciones, con
  conversión automática mostrada en vivo según el precio actual.
- Tipo de orden: al menos mercado y límite (verificar en el SDK qué tipos soporta Hyperliquid
  exactamente antes de prometer más de lo que la API permite).
- **Precio de entrada estimado**: mostrar antes de confirmar, calculado con el mark/mid actual (o el
  precio límite introducido).
- **Precio de liquidación estimado**: calcular y mostrar ANTES de enviar la orden, no solo después
  — es el dato de riesgo más importante y el usuario debe verlo antes de confirmar, no descubrirlo
  después de abierta la posición.
- Stop Loss y Take Profit: inputs de precio (o de % de distancia, más cómodo para muchos usuarios),
  usando los trigger orders de Hyperliquid (stop market / take profit market) — verificar en el SDK
  el método/endpoint exacto antes de asumir el nombre.
- Confirmación explícita antes de enviar cualquier orden real (no auto-submit por una tecla o click
  accidental) — resumen de la orden completa (par, lado, tamaño, leverage, entrada, liquidación,
  SL/TP) y un paso de "¿confirmas? y/n" o equivalente en ratón, mínimo.

**Panel de posición/órdenes abiertas (una vez hay posición o solo con órdenes pendientes):**
- Lista de posiciones abiertas: par, lado, tamaño, apalancamiento, precio de entrada, precio de
  liquidación (recalculado en vivo si aplica), PnL no realizado (en USD y en %), con color
  verde/rojo consistente con el resto de la app.
- Lista de órdenes abiertas (límite, SL, TP pendientes de ejecutar): par, tipo, precio, tamaño,
  con opción de cancelar individualmente.
- Botón/tecla de cierre rápido de posición (a mercado), con su propia confirmación.
- Botón/tecla de edición de SL/TP de una posición ya abierta, sin tener que cerrarla y reabrirla.

**Interacción — teclado Y ratón, ambos de primera clase:**
- Navegación por teclado consistente con el resto de la app (mismo patrón de flechas/Tab/Enter que
  ya usan las otras 8 vistas).
- Click del ratón para: seleccionar campos de la orden, ajustar apalancamiento (si hay
  slider/stepper), seleccionar una posición/orden de la lista, confirmar acciones (botones
  clicables además de sus equivalentes de teclado).
- Ningún control debe ser SOLO de ratón o SOLO de teclado — todo accesible por ambos caminos.

- Todas las órdenes de este panel se firman con la agent wallet, no con WalletConnect (ver sección
  de Firma más arriba) — WalletConnect/MetaMask solo interviene en depósito, retiro y autorización
  de la agent wallet, nunca en el día a día de trading.

**Orden de dependencia — no construir esto antes de lo anterior:**
Este panel depende de que la agent wallet ya esté autorizada (pasos 3-6 del orden de construcción
de Fase 2 más abajo: depósito pequeño en mainnet → faucet testnet → autorización de agent wallet).
Se puede construir la UI/maquetación de este panel de forma aislada antes (con datos de prueba, sin
conectar aún al Exchange API real), pero la funcionalidad real de enviar órdenes no puede probarse
hasta tener la agent wallet lista contra testnet. Verificar en qué paso del orden de Fase 2 se está
antes de lanzar esta sesión.

### Seguridad — no negociable
- La agent key NUNCA se commitea a git. Añadir su ruta de almacenamiento al `.gitignore` desde el
  primer commit de esta fase, antes de que exista el archivo.
- Ningún log debe imprimir la agent key ni ningún material de firma en texto plano.
- Confirmar dos veces antes de: retiros, y la primera orden real contra mainnet.
- El precio de liquidación estimado SIEMPRE debe mostrarse antes de confirmar una orden con
  apalancamiento, nunca solo a posteriori.

## Señal descartada
La "densidad de liquidación por OI-delta" (portada de liq.pine) se implementó (`src/liqdens.rs`)
pero se descartó como inútil en la práctica: requiere ~61 velas de calendario real acumulando OI
en memoria (no hay histórico de OI en la API de Hyperliquid, solo de precio), y el bug de latencia
de precio (ya resuelto) contaminaba cualquier lógica de "la vela cruza el nivel". No recuperar salvo
que haya un poller de OI persistente en background (ej. en el RedMagic) que resuelva el warmup real.

## Lo ya construido (Fase 1, completa)
- **Capa de datos** (`src/data/`): poller de `metaAndAssetCtxs` cada 5s (funding, OI, premium,
  oracle), WebSocket `allMids` + suscripción `bbo` dinámica por par seleccionado (sub-segundo),
  fallback por staleness (`MID_STALE` 15s) si el WS calla, fetcher de velas + funding histórico.
- **Vista 1, Ranking cross-pair**: tabla ordenable (`s`) — precio, 24h%, funding horario/APR,
  premium en bps, OI notional, ΔOI 5m/1h, clasificación de flujo (cruce OI×precio).
- **Vista 2, Par**: velas OHLC reales (verde/rojo consistente), eje de precio con gridlines,
  hover con ratón mostrando OHLC/vol/antigüedad, temporalidad configurable (`i`: 15m/1h/4h/1d),
  historial de funding, sparklines de OI/mid, RSI(14) + ADX/DMI(14).
- **Vista 3, Heatmap top-OI**: top 30 pares por OI, color por funding APR / ΔOI 1h / cambio 24h
  (`m` cambia métrica).
- **Vista 4, Whale positioning**: leaderboard (top-100) + `clearinghouseState` individual por
  dirección, ciclo de 60s, emisión parcial cada 10 cuentas escaneadas.
- **Vista 5, Wallet watch-only**: introducir cualquier dirección pública (`e`), poll cada 10s de
  saldo/margen/posiciones con distancia a liquidación usando el mark en vivo del propio TUI.
- **Vista 6, Heatmap de liquidaciones**: estimación por OI×volumen×tramos de leverage, etiquetada
  ESTIMADO, con rombos ◆ para liquidaciones exactas de whales conocidas.
- **Vista 7, Ballenas + RSI/ADX/DMI**: port de `reference/whales_rsi_adx_dmi.pine`, líneas
  RSI/MA/%B/ADX/±DI, columnas de intensidad + marcas ▲▼, checklist de filtros en vivo, log de
  últimos disparos.
- **Señales** (`src/signals.rs`): funciones puras con tests unitarios.
- **`cargo run --bin probe`**: sonda sin TUI para validar REST + WS de forma aislada, con desglose
  de cadencia por tipo de suscripción.
- Nota técnica: el SDK de crates.io (0.6.0) no expone `metaAndAssetCtxs` (funding/OI). Pineado a
  `master` del repo oficial (rev `aac75585`) en `Cargo.toml` — NO revertir a la versión de crates.io
  sin verificar primero que ese endpoint ya esté publicado ahí.
- Nota de comportamiento (no bug): columnas ΔOI (ranking y heatmap) muestran "—" hasta que la app
  lleva corriendo el tiempo de la ventana (5m/1h) porque el historial de OI es en memoria, no
  persistido — no "arreglar" esto pensando que es un fallo.

## Repos de referencia
- Base de estructura TUI (esqueleto reusado, no fork literal): https://github.com/harsh-vardhhan/option-analysis
- SDK de datos/ejecución: https://github.com/hyperliquid-dex/hyperliquid-rust-sdk
  - Pineado a `master` (rev `aac75585`) por el endpoint `metaAndAssetCtxs`.
  - Exchange API (REST) para órdenes — verificar métodos exactos de trigger orders (SL/TP) antes
    de asumir nombres.
  - Espera un `LocalWallet` (private key en memoria) — es lo que usará la agent wallet.
- WalletConnect Rust (dApp side): `walletconnect_client` (docs.rs/walletconnect-client) — candidato
  principal, cobertura de firma post-conexión sin confirmar, ver spike obligatorio arriba.

## Decisiones de arquitectura ya tomadas

### Firma: MetaMask (WalletConnect) + agent wallet, no login por email
- Login por email en Hyperliquid descartado: la clave privada la custodia el backend de Hyperliquid.
- MetaMask conectado vía WalletConnect v2 es la cuenta MAESTRA: depósitos, retiros, autorización de
  agent wallet.
- Día a día (trading) se firma con la agent wallet (permiso de trading, sin retiro), sin pasar por
  WalletConnect en cada orden.
- Distinto de la wallet en modo seguimiento de Fase 1 (Vista 5): esa es solo lectura de direcciones
  públicas, sin relación con custodia de fondos propios.

### Latencia
- Latencia esperada: ~0.2–0.9s, dominada por procesamiento server-side de Hyperliquid.
- Fix aplicado en Fase 1: suscripción `bbo` por-par seleccionado va sub-segundo; `allMids` (231
  pares a la vez) va a ~5s por diseño del servidor, no es un bug.

### Transporte
- WebSocket: bbo por-par, allMids, order book, user events (lecturas en vivo).
- REST: todo lo demás, incluyendo escritura de órdenes (place/cancel/modify).
- WalletConnect: solo para las firmas de cuenta maestra (depósito, retiro, autorización de agent).

## Señales / indicadores (prioridad: datos nativos de Hyperliquid > TA derivado de precio)
- Funding rate cross-pair (ranking de extremos)
- OI delta (open interest creciente/decreciente vs. dirección de precio)
- Mapa de niveles de liquidación (estimado, Vista 6)
- Panel Ballenas + RSI/ADX/DMI (Vista 7, TA puro sobre precio, no OI/on-chain)
- Whale positioning vía leaderboard + clearinghouseState por dirección (Vista 4)
- Cruces entre pares: correlación de funding BTC/ETH/alts, ranking de fuerza relativa (pendiente)
- RSI/ADX/DMI: solo como confirmación secundaria, nunca señal primaria
- **Vista de Flujo de Dinero / Posicionamiento** (nueva, ver detalle abajo)

### Detalle: Vista de Flujo de Dinero / Posicionamiento (Vista 6 con numeración actual, solo lectura)
Objetivo: responder "¿hacia dónde se mueve el dinero entre pares?" y "¿está el barco demasiado
cargado hacia un lado?" (crowd positioning → señal contraria). Todo con datos ya recolectados o
una suscripción WS nueva — es Fase 1 en espíritu (solo lectura), puede avanzar en paralelo a los
pasos de fondos de Fase 2 sin conflicto.

Componentes, por prioridad de construcción:

1. **Rotación de capital (ΔOI notional cross-pair)** — con datos YA disponibles del poller de 5s:
   - Ranking de pares por ΔOI en USD absolutos y en % de su OI, ventanas 1h/4h/24h.
   - Distinguir: OI subiendo con volumen alto = posicionamiento con convicción; OI subiendo con
     volumen bajo = acumulación silenciosa (marcar ambos casos de forma distinta).
   - Nota: hereda la limitación del historial en memoria (ventanas largas requieren uptime o el
     futuro poller persistente en el servidor de fondo) — mostrar "—" honesto mientras no haya datos.

2. **Desequilibrio long/short (proxies, no hay ratio global directo en la API)**:
   - **Funding como termómetro**: percentil del funding actual contra su propio histórico (ampliar
     el histórico de funding que ya se descarga de ~3d a ~30d para que el percentil sea significativo
     — verificar hasta dónde permite el endpoint fundingHistory). Funding en percentil extremo
     (>p95 o <p5) = bandera de crowd sobrecargado.
   - **Skew de whales**: agregar de la Vista 4 (ya recolectado) la suma de notional long vs. short
     de las top-100 por par → "% long de whales". El CONTRASTE es la señal estrella: funding alto
     (masa long) + whales netas cortas = setup contrario clásico. Mostrar ambos lado a lado.
   - **Premium sostenido**: ya se muestra en bps; añadir su persistencia (media de la ventana, no
     solo el instantáneo) como confirmación de presión agresiva de un lado.

3. **Asimetría de liquidaciones**: del heatmap estimado de Vista 6, derivar un número simple por
   par: ratio de notional liquidable a ±3% por encima vs. por debajo del precio actual. Más
   combustible abajo que arriba = camino de menor resistencia bajista (y viceversa).

4. **CVD (Cumulative Volume Delta)** — requiere suscripción WS nueva al canal `trades` del par
   seleccionado (mismo patrón que la suscripción bbo dinámica ya existente): clasificar cada trade
   por lado agresor y acumular el delta. Divergencia CVD/precio (CVD sube, precio plano) = absorción.
   Empezar solo con el par seleccionado, no los 231 (coste de WS).

5. **Score compuesto de sobreextensión** (última pieza, cuando 1-4 existan): por par, contar cuántos
   extremos coinciden en el mismo lado (funding extremo + premium sostenido + asimetría de liqs +
   divergencia CVD + whales en contra). No es una señal de entrada automática — es un ranking de
   "pares donde el barco va más cargado", para revisión humana. Coherente con el framework del
   usuario: régimen → dirección/combustible → gatillo de agotamiento → objetivo en cluster de
   liquidaciones opuesto.

UI: Vista de Flujo, siguiendo el patrón de las demás. Layout sugerido: tabla principal
de rotación (componente 1) arriba, panel de posicionamiento del par seleccionado (componentes 2-4)
abajo, columna de score (5) integrada en la tabla. Reusar el selector de par global existente.
✅ COMPLETA Y VERIFICADA — ver "Orden de construcción" para el detalle de qué quedó hecho.

## Orden de construcción

### Fase 1 — completa
Todas las vistas 1-7 construidas y validadas en vivo contra mainnet. Ver "Lo ya construido" arriba.

### Fase 2 — en curso
1. Spike de WalletConnect (pairing + firma de prueba) — ✅ hecho, SPIKE OK validado con móvil
2. Conexión MetaMask (QR) + cuenta maestra visible en la nueva pestaña de Fondos — ✅ hecho (Vista 8)
2.5. Saldo real de Hyperliquid (Vista 8) — ✅ hecho, reusa clearinghouseState de Vista 9
2.6. Saldo on-chain de USDC en Arbitrum (Vista 8, Pieza 1) — ✅ hecho y confirmado por el usuario
   con su wallet real conectada (~0.58 USDC visibles)
3. **✅ DEPÓSITO REAL EJECUTADO Y CONFIRMADO** — el usuario depositó 5 USDC reales vía la Vista 8
   (tecla `p`) y lo verificó de forma independiente conectando la misma wallet a la web oficial de
   Hyperliquid, confirmando el saldo ahí también. Primera transacción de fondos reales del proyecto,
   cerrada con éxito.
4. **✅ Faucet de testnet reclamado** — el usuario tiene ~999 USDC mock en su cuenta de Hyperliquid
   testnet (confirmado en el navegador, no visible en el TUI porque este apunta a mainnet por
   defecto — usar `--testnet` para verlo/operarlo).
5. **✅ Código de retiro construido (funciona para mainnet y testnet, misma función, tecla `w` en
   Vista 8), ⚠️ VALIDACIÓN REAL PENDIENTE.** Firma EIP-712 gasless (`eth_signTypedData_v4`, aparece
   en MetaMask como "Signature request", no como transacción) → POST del action `withdraw3` a
   `/exchange`. Garantías verificadas antes de firmar de verdad: el hash EIP-712 que firmará
   MetaMask coincide byte a byte con el del SDK oficial (test unitario), el action serializado es
   idéntico al del SDK, y una sonda real contra `/exchange` de testnet con firma dummy respondió
   "Unable to recover signer" (confirma que el formato del wire llega correcto hasta la verificación
   de firma). Hallazgo importante: el bridge de testnet no reparte USDC de Circle sino su propio
   mock "USDC2" (`0x1baa…34d5`) — el vigilante de llegada ya usa ese contrato en testnet.
   **Pendiente de que el usuario ejecute la validación real** (necesita su móvil): `cargo run --
   --testnet` → Vista 8 → `w` → cantidad de prueba → comparar destino contra MetaMask → aprobar la
   "Signature request" (debe decir `HyperliquidTransaction:Withdraw`) → esperar confirmación
   (~5 min). Solo tras validar esto en testnet, repetir el mismo flujo sin `--testnet` para retirar
   de mainnet si se desea.
5.5. **⚠️ BLOQUEANTE REAL, CORREGIDO EL DIAGNÓSTICO: la cuenta del usuario está en "Unified Account
   Mode".** Hyperliquid rechazó la transferencia con "action disabled when unified account is
   active" — investigado: en este modo spot y perps comparten un único fondo de margen
   automáticamente, no hace falta transferir nada. Ver sección completa "CORRECCIÓN CRÍTICA:
   Unified Account Mode" más arriba para el detalle completo de qué corregir (detectar el modo de
   cuenta, usar spotClearinghouseState como fuente de verdad de margen si es unificada, ocultar la
   tecla `t` en ese caso). Este es ahora el bloqueante real antes del punto 7 — sin corregir esto,
   el panel de ejecución calculará mal el margen disponible en la cuenta real del usuario (que ya
   sabemos está en modo unificado en mainnet).
6. **Autorización de agent wallet — ya ejecutada al menos una vez contra MAINNET por el usuario**
   (confirmado: el modal mostró "invalida el agent anterior", señal de que ya había una autorización
   previa en mainnet; firma verificada en MetaMask con el tipo correcto `HyperliquidTransaction:
   ApproveAgent"). Pendiente de que el usuario confirme que el flujo terminó con "autorizado y
   verificado" en el TUI, y de repasar los puntos de seguridad de la clave (permisos 0600, `.gitignore`
   cubriendo `secrets/`) antes de dar este paso por completamente cerrado. La agent key generada se
   guarda localmente, fuera de git, y se usa como `LocalWallet` normal del SDK para firmar órdenes
   del día a día sin pasar por WalletConnect en cada trade.
7. **✅ HECHO contra TESTNET**: Panel de ejecución conectado al Exchange API real, firmado con la
   agent wallet — órdenes IOC construidas a mano (el SDK asume que la wallet firmante tiene las
   posiciones, cosa que no aplica con agent wallet), posiciones/órdenes reales leídas de
   `clearinghouseState`/`frontendOpenOrders`, margen validado con `perps_avail()` (fuente correcta
   para cuenta unificada). Candado de seguridad ya existente: `trader::spawn` se niega a armarse
   contra cualquier red que no sea testnet con `secrets/agent_testnet.json` presente — mainnet
   sigue siendo maqueta a propósito.
7.5. **✅ CÓDIGO HECHO (2026-07-30), ⚠️ VALIDACIÓN REAL PENDIENTE: panel de ejecución real contra
   MAINNET.** `trader::spawn` acepta ahora testnet Y mainnet como rutas explícitas separadas (match
   por red; cualquier otra se rechaza); `main.rs` carga `secrets/agent_testnet.json` con `--testnet`
   y `secrets/agent_mainnet.json` sin él (el loader ya verificaba el `hyperliquid_chain` del
   archivo, así que una key de testnet nunca firma contra mainnet). Fricción extra SOLO mainnet:
   el modal de confirmación exige teclear CONFIRMO + Enter (`y`, Enter a secas y el click del botón
   no ejecutan; solo Esc cancela). Cautelas previas intactas (liq. estimada visible, mínimo $10,
   margen con `perps_avail()`). Test `mainnet_exige_frase_confirmo` cubre el flujo. Nota: el bug de
   conectividad de MetaMask (abajo) NO bloquea el trading diario (firma con la agent key local, sin
   WalletConnect), pero sí depósitos/retiros — resolverlo antes de mover más fondos.
   IMPORTANTE (descubierto por el usuario en producción): el panel de ejecución real NO requiere
   sesión WalletConnect activa para armarse ni para leer/operar la cuenta — solo necesita el
   archivo secrets/agent_mainnet.json en disco. Esto es coherente con el propósito de la agent
   wallet (operar sin fricción de móvil en cada trade), pero cambia el modelo de amenaza: cualquiera
   con acceso a la laptop desbloqueada puede abrir hyperT y operar con fondos reales sin necesitar
   el móvil, con la frase CONFIRMO como único freno. Confirmado explícitamente aceptable por el
   usuario dado que la clave nunca se sube a git (`.gitignore` con `/secrets/`, verificado con
   `git check-ignore` y con `git log --all --full-history -- secrets/` vacío). Pendiente: primera
   orden real MUY pequeña con supervisión activa.
8. Validar todo el flujo completo en TESTNET de principio a fin (depósito ya no aplica ahí, pero
   sí: agent wallet + abrir posición + SL/TP + cerrar posición + retiro)
9. Repetir contra MAINNET con fondos reales, con supervisión activa en cada paso — el usuario ya
   tiene 5 USDC reales dentro de Hyperliquid listos para esto
10. Journal Obsidian + backtesting (integración externa, prioridad baja)

### Trabajo paralelo (solo lectura, sin conflicto con Fase 2)
- **Vista de Flujo de Dinero / Posicionamiento** — ✅ completa y verificada (rotación ΔOI, funding
  percentil 30d, skew whales, asimetría liqs, CVD, score compuesto).
- **Pulido Vista 2 (gráfico de par)** — ✅ completo: velas sólidas con medios bloques, sub-panel
  RSI/ADX/DMI apilado (ahora vía renderizado de imagen real, ver spike Kitty más abajo), 7
  temporalidades (1m-1d).
- **Renderizado de imagen real (ratatui-image + plotters) para paneles de indicadores** — ✅
  implementado en Vista 2 (sub-panel) y Vista 3 (Ballenas+RSI/ADX/DMI), con caché y fallback
  halfblocks. Confirmado visualmente por el usuario en su Kitty real.
- **Delta por vela** (Vista 2) — ✅ implementado y validado (100 tests en verde), mismo pipeline de
  renderizado de imagen, reusa la suscripción de trades del CVD.
- **i18n (interfaz en inglés por defecto + toggle ES/EN)** — EN CURSO. Infraestructura + chrome
  universal (header, footer, ayuda, buscador) migrados y verificados en runtime. Las 7 vistas de
  lectura (ranking, heatmap, whales, liq, flow, wallet, pair) también migradas y verificadas.
  PENDIENTE (recuento real verificado 2026-08-05, menor de lo estimado originalmente):
  fondos.rs ~92 literales, exec.rs/ui/exec.rs ~21 — i18n::t() ya usado 16/17/6 veces
  respectivamente, así que está parcialmente migrado, no desde cero. Dejar
  para sesión aparte por ser los paneles que tocan dinero real, con atención extra a que ningún
  texto de confirmación/cantidad/dirección quede ambiguo al traducir.

### Buscador/filtro rápido de par (estilo nvim `/`) — Vista 1 y Vista 6
Con 232+ pares, hacer scroll manual para encontrar uno concreto es lento. Añadir un buscador
incremental en Ranking (Vista 1) y Flujo (Vista 6):
- Tecla `/` abre una ventana pequeña (overlay, no ocupa toda la pantalla) con un campo de texto.
- Al escribir (ej. "HYPE" o "ETH"), la tabla de abajo se filtra en vivo a los pares cuyo ticker
  contiene el texto (substring, no exacto — "ETH" debe seguir mostrando ETH y cualquier otro que
  lo contenga si existiera, tipo fuzzy simple, no hace falta fuzzy matching completo tipo fzf).
  Coincidencias que empiezan por el texto arriba de las que solo lo contienen en medio.
- `Enter` selecciona el primer resultado y cierra el buscador (o navega directo a Vista 2 con ese
  par, a decidir cuál es más natural — probablemente cerrar el buscador y dejar el cursor en esa
  fila, sin saltar de vista automáticamente).
- `Esc` cierra el buscador sin aplicar filtro, vuelve a la tabla completa.
- Mismo componente reusado entre Vista 1 y Vista 6 — es lógica de filtrado sobre una lista de
  tickers, no algo específico de una vista.
✅ HECHO — buscador `/` implementado (src/search.rs, src/ui/search.rs).

### Filtro/ranking por "combustible" direccional — Vista 6 (Flujo) — ✅ HECHO, verificado en código
Confirmado por grep directo sobre src/flow.rs y src/app.rs (2026-08-03): implementado y MÁS
completo de lo que esta sección original pedía. `FlowSort` cicla tres modos: `Rotation → Fuel →
Confluence → Rotation`.
- **Fuel**: ordena por `liq_asym()` — asimetría normalizada en [−1,+1] del combustible de
  liquidación ±3% (positivo = más combustible ABAJO = sesgo bajista, negativo = alcista, igual
  convención que se pidió). Se muestra en la tabla como celda "▼64/36" (reparto % abajo/arriba).
- **Confluence** (no pedido explícitamente en el texto original, construido de más): ordena por
  una clave que exige que el score compuesto Y la asimetría de combustible apunten al MISMO lado
  (`(net > 0.0) != (fuel_dir > 0.0)` descarta si no coinciden) — exactamente la idea de "priorizar
  pares donde varias señales confluyen" que se sugería como posible extensión.
- Honestidad de datos ya cubierta con test: pares sin combustible a ningún lado (`liq_asym(0,0)`)
  devuelven `None` y van al final, nunca se tratan como cero en el orden.
- Pendiente solo de que el usuario lo pruebe visualmente en su Kitty (ciclar con la tecla de orden
  ya existente en Vista 6 y confirmar que los tres modos se comportan como espera).

### Sincronizar ventana temporal entre Vista 2 y Vista 3 (mismo rango de fechas visible)
✅ HECHO — ambas vistas consumen `pair::visible_candles()` como única fuente de verdad; verificado
en vivo a dos anchos de terminal distintos, misma primera vela y mismo conteo en ambas vistas.

## Nuevo indicador: Delta por vela (paso previo al Footprint Chart)
✅ HECHO — ver "Trabajo paralelo" arriba. Contexto: framework de "order flow" (Volume Delta, CVD,
OI, Footprint) discutido en otra conversación con el usuario sobre un artículo de Twitter. De los 4
pilares del framework, 3 ya estaban construidos sin planearlo así (CVD, su divergencia, OI+ΔOI), y
el Delta por vela cubre un cuarto de forma simplificada. Solo falta el Footprint Chart completo
(volumen por nivel de precio dentro de cada vela) — NO construir todavía, es la extensión natural
si el Delta por vela demuestra ser útil en uso real primero.

## Nueva feature: ver dirección completa de whale para copiar (Vista 7, Whale positioning)
✅ HECHO — modal con Enter/click, dirección completa sin truncar, copia al portapapeles vía OSC 52
(Kitty/Ghostty/WezTerm) usando `base64ct` ya presente en el árbol, sin dependencia nueva.

## Nueva feature: wallets relacionadas / rastreo de fondos (Vista 9, Wallet watch-only)
Objetivo del usuario: al observar una wallet, poder ver desde qué otras direcciones recibió fondos
y a cuáles envió, para hacer seguimiento de "smart money" moviéndose entre cuentas.

**Alcance decidido — solo transferencias DENTRO de Hyperliquid, no rastreo on-chain:**
Rastrear el origen de fondos antes de llegar a Hyperliquid (quién fondeó la wallet en Arbitrum,
etc.) requeriría un explorador de bloques externo (Arbiscan API u otro) y crecería
exponencialmente con cada salto hacia atrás — eso ya no son "datos nativos de Hyperliquid" y es un
proyecto de rastreo on-chain aparte, fuera de alcance por ahora. Lo que SÍ es factible y coherente
con la prioridad "datos nativos > TA/externo" del proyecto: usar el endpoint de historial de
movimientos no relacionados con trading de Hyperliquid (`userNonFundingLedgerUpdates` o el nombre
exacto que tenga en el SDK/API — verificar antes de asumir) para listar depósitos, retiros, y
**transferencias internas entre direcciones de Hyperliquid** (la acción `usdSend`/`spotSend`) de la
wallet en observación. Esto da un salto directo: quién le envió fondos a esta wallet, y a quién
esta wallet le envió fondos — con cantidad y fecha exactos.

**Diseño de UI — lista navegable con pivote, NO un árbol gráfico estático:**
Un árbol visual completo no cabe bien en un TUI más allá de 1-2 niveles y crece sin control si la
wallet observada es una whale con muchas contrapartes. En su lugar:
- Dos listas nuevas en Vista 9 (sección aparte, bajo lo ya construido): "Fondos recibidos de" y
  "Fondos enviados a", cada fila con dirección (truncada, reusar el patrón de modal de dirección
  completa ya construido en Vista 7 para poder ver/copiar la dirección entera), importe y fecha.
- Al pulsar `Enter` sobre una dirección de esas listas, esa dirección se convierte en la NUEVA
  wallet observada (mismo mecanismo que ya existe para introducir una dirección manualmente con
  `e`, pero disparado desde aquí en vez de tecleando) — permite "caminar" el grafo de fondos salto
  a salto, como navegar enlaces en un explorador de bloques.
- Mantener una pila de navegación (historial de wallets visitadas) con una tecla para "volver atrás"
  (ej. `Backspace` o similar, revisar bindings existentes para evitar colisión) — para no perder el
  hilo al explorar varios saltos.
- Límite de resultados por lista (ej. los N movimientos más recientes o más grandes) si el endpoint
  no pagina bien o si una whale tiene demasiados movimientos — no intentar cargar un historial
  ilimitado de golpe.
- Solo lectura, no toca fondos ni firma — se puede lanzar sin supervisión activa.


✅ HECHO — verificado contra código el 2026-08-05 (src/ui/wallet.rs), con 5 tests en verde
(resumen_win_rate_y_pnl, apertura_detecta_flip_de_lado, apertura_es_cota_inferior_si_no_hay_cruce_por_cero,
entre otros). Nota de numeración: Wallet watch-only es la **Vista 9** con la numeración actual.

Requiere una llamada nueva: `userFills` (historial de operaciones/fills de la dirección en
observación) — no confundir con `clearinghouseState` (que solo da el estado actual, sin histórico).

**Layout exacto pedido por el usuario, de arriba a abajo:**

1. **Bloque de resumen, arriba de todo**: win rate derivado de los fills cerrados (% de operaciones
   con `closedPnl` positivo vs. negativo), con etiqueta clara de si la wallet es "ganadora" o
   "perdedora" según ese porcentaje (ej. umbral simple: >50% ganadora, <50% perdedora — a afinar).
   Junto a esto, el **PnL realizado acumulado histórico** (suma de todos los `closedPnl` de los
   fills disponibles) — da la imagen de rentabilidad total de esa dirección, no solo de la posición
   puntual que se ve abajo.
2. **Posiciones abiertas** (ya existente, del `clearinghouseState` actual) — navegable con
   flechas/teclado, igual que el resto de tablas de la app. Al pulsar `Enter` sobre una posición
   seleccionada, abrir un modal pequeño (mismo patrón que el modal de dirección de whale ya
   construido) mostrando:
   - **Fecha/antigüedad de apertura** de esa posición — cruzando `clearinghouseState` (tamaño/lado
     actual) contra `userFills` para encontrar el/los trade(s) que originaron el tamaño actual neto.
   - **Funding acumulado pagado/cobrado** desde la apertura (`cumFunding.sinceOpen` — signo YA
     VERIFICADO empíricamente, ver sección de i18n/tareas más abajo: positivo = PAGADO).
3. **Operaciones cerradas**, en su propia sección SEPARADA de las abiertas (no mezclar en la misma
   tabla) — lista de los últimos N fills cerrados de esa dirección, cada uno con su fecha y su
   **ROE%** (retorno sobre margen, no solo el PnL absoluto — más comparable entre operaciones de
   distinto tamaño).

**Notas de implementación:**
- Cruzar fills para determinar "apertura de la posición actual" es una heurística, no un dato
  exacto garantizado por la API (una posición puede haberse ido ampliando/reduciendo con varios
  fills a lo largo del tiempo) — documentar esto en el código y, si hay ambigüedad, mostrar la
  fecha del fill más antiguo relevante en vez de asumir un único trade de apertura.
- `userFills` puede tener paginación o límite de resultados — revisar el comportamiento real del
  endpoint (mismo criterio de verificación que se ha aplicado a otros endpoints en este proyecto:
  no asumir, comprobar con una llamada real primero).
- Es solo lectura, no toca fondos ni firma — se puede lanzar sin supervisión activa.
- Reusar el patrón visual de modal ya establecido (overlay centrado con `Clear`, cerrable con
  `Esc`/`Enter`/click) para el modal de fecha+funding de la posición abierta.

## ✅ HECHO: wallets relacionadas en Vista 9 (fondos recibidos de / enviados a)
Endpoint VERIFICADO con curl contra mainnet antes de construir: `userNonFundingLedgerUpdates`
(POST /info, `startTime` OBLIGATORIO, respuesta más ANTIGUA primero — se invierte). El SDK pineado
no lo expone → POST crudo con el `info_post` ya existente. Se pide en el mismo watcher y a la misma
cadencia que `userFills` (60s).
- Solo se muestran los movimientos CON contraparte: `internalTransfer`/`subAccountTransfer` y
  `spotTransfer`/`send` (par `user`→`destination`), y `vaultDeposit`/`vaultWithdraw`/
  `vaultDistribution` (contraparte = `vault`, sentido según el tipo). Los `deposit`/`withdraw` del
  bridge, `liquidation`, `accountClassTransfer` y `spotGenesis` NO relacionan la cuenta con otra
  wallet y se descartan a propósito — no es que falten.
- Un `send` de la cuenta a sí misma (mover entre dexes) también se descarta.
- UI: dos tablas lado a lado bajo las posiciones (fecha · wallet abreviada · cantidad · tipo).
  `Tab` cicla el foco entre posiciones → recibidos → enviados (en Vista 9 con dirección observada,
  `Tab` YA NO cambia de vista; sin dirección observada sigue ciclando vistas como siempre).
- `Enter` sobre una fila abre el modal de dirección completa YA EXISTENTE (el de whales, ahora
  compartido como `ui::whales::draw_addr_overlay`); ahí `Enter` PIVOTA la wallet observada a esa
  dirección, `c` copia por OSC 52, `Esc`/click cierran. El click nunca pivota: solo abre/cierra.
- `Backspace` vuelve a la wallet anterior (pila `wallet_back`, profundidad visible en la cabecera).
  Teclear una dirección a mano con `e` reinicia la pila (es una raíz nueva).
- Solo lectura; no toca `src/wallet/` ni `trader.rs`. Validado en vivo con el driver pty contra una
  cuenta real de mainnet (pivoteo y vuelta atrás incluidos) + tests unitarios de parseo y navegación.

## ✅ RESUELTO: input de wallet (Vista 9) siempre vacío + pegado de portapapeles
El input de añadir wallet (tecla `e`) ya no pre-rellena con la última dirección (`input_buf =
String::new()` en `start_input`). Pegado desde portapapeles soportado vía bracketed paste
(`EnableBracketedPaste`/`Event::Paste`), filtrando a caracteres válidos de dirección (0x + hex,
tope 42) — funciona con Ctrl+V y click-medio en Kitty.

## ✅ RESUELTO: signo de `cumFunding.sinceOpen` verificado empíricamente
Conclusión con evidencia exacta (reconciliación byte a byte contra `userFunding.usdc` en una cuenta
con historia completa bajo el cap de 500 eventos): **`sinceOpen` > 0 = el trader PAGÓ funding;
< 0 = lo COBRÓ** — el signo contrario a la intuición inicial basada solo en teoría del mecanismo.
La UI ya restaura el color/etiqueta (rojo=pagado / verde=cobrado) con la prueba documentada en el
código. Aplicar esta misma convención en la ampliación de Vista 9 (historial de fills) cuando se
construya.

## Internacionalización: interfaz del TUI en inglés, con opción de idioma ES/EN
EN CURSO — ver estado real en "Trabajo paralelo" arriba. Módulo central `src/i18n.rs`: enum
`Lang { En, Es }`, estado global (inglés por defecto), `t() -> &Strings`, selección vía
`--lang=es/en` (main.rs) o tecla `L` para alternar en vivo (global, app.rs). Patrón: campos en
structs const `EN`/`ES` dentro de `Strings`, sustituyendo literales por `i18n::t().campo` — NO
literales sueltos repartidos por `src/ui/`.
- Importante: esto es SOLO sobre los textos que ve el usuario dentro del TUI. NO afecta a cómo se
  le dan instrucciones a Claude Code en las sesiones de desarrollo — eso sigue en español, y este
  CLAUDE.md también sigue en español.
- Pendiente: `fondos.rs` (~246 literales) y `exec.rs` (~108) con sus enum-labels de órdenes —
  sesión aparte, con atención extra a textos de confirmación/cantidad/dirección.

## ⚠️ BUG (probablemente intermitente/puntual, no reproducido de nuevo): fallo de conectividad al
## conectar MetaMask (Vista 8)
Estado actualizado: el fallo original no volvió a reproducirse. El usuario confirmó conexión
exitosa tanto con MetaMask como con Rabby Wallet en un intento posterior, sin cambios de código de
por medio para este bug en concreto (el spike aislado de diagnóstico descartó relay, projectId, QR,
proveedor rustls y silenciado de errores — todo sano). Conclusión provisional: probablemente fue un
fallo puntual (QR caducado antes de escanear, relay con hiccup momentáneo, o similar), no un
problema estructural del código de conexión. Dejar esta nota como referencia — si el fallo
reaparece, reproducirlo con el detalle exacto pedido (mensaje de la tira, en qué paso se detiene)
antes de asumir la misma causa.

## ⚠️ BUG: las velas de Vista 2 no se refrescan en vivo mientras permaneces en la vista
Reportado por el usuario: en Vista 2 (Par), con temporalidad de 1 minuto, no aparecen velas nuevas
mientras te quedas mirando la vista — solo se actualizan si sales a otra vista y vuelves a entrar
en Vista 2. Es más notorio en 1m (donde debería verse una vela nueva cada minuto), pero
probablemente afecta a todas las temporalidades igual, solo que se nota menos cuanto más larga es
la vela.

**Causa probable (a confirmar antes de arreglar):** el fetch de velas parece estar disparado solo
por eventos puntuales (entrar a la vista, cambiar de par, cambiar de temporalidad con `i`), no por
un timer/tick periódico mientras ya estás dentro de la vista — así que aunque haya pasado tiempo
suficiente para que exista una vela nueva en el servidor, la app no vuelve a pedirla hasta el
próximo evento que dispare un fetch.

**Qué corregir:**
- Añadir un re-fetch periódico de velas mientras Vista 2 está activa, con una cadencia razonable
  (no hace falta pedir cada segundo — con pedir cada ~10-15s alcanza para que 1m se sienta "en
  vivo" sin sobrecargar la API; ajustar el intervalo según convenga).
- Evaluar si conviene condicionar el re-fetch a la temporalidad activa: en 1m tiene sentido refrescar
  con más frecuencia que en 1d, donde una vela nueva tarda 24h en aparecer y no hace falta pedir
  cada pocos segundos.
- Verificar que este cambio no rompe el caché/comportamiento ya existente de las imágenes
  rasterizadas (RSI/ADX/DMI, delta por vela) — esos paneles ya tienen su propio criterio de caché
  (revalidar solo si cambia el tamaño o llega vela nueva, ≤1/min); confirmar que un fetch de velas
  más frecuente no dispare re-rasterizados innecesarios de esos paneles si no ha cambiado nada
  realmente relevante para ellos.
- Solo lectura, no toca fondos ni firma — se puede lanzar sin supervisión activa.

## ⚠️ BUG: artefacto gráfico al cerrar el modal de ayuda (?) sobre Vista 2 / Vista 3
Reportado por el usuario: al abrir la ventana de ayuda (`?`) estando en Vista 2 (Par) o Vista 3
(Ballenas+RSI/ADX/DMI) — las dos vistas con paneles renderizados como imagen real vía
`ratatui-image` + protocolo Kitty (RSI/ADX/DMI, delta por vela) — al CERRAR la ayuda queda un
resto/artefacto incrustado sobre la gráfica, en vez de que el panel se redibuje limpio.

**Causa probable (a confirmar, no asumir sin investigar):** el modal de ayuda se dibuja y cierra
como celdas de texto normales de ratatui (widget `Clear` + redraw), pero el contenido de los
paneles de Vista 2/3 no es texto — es una imagen transmitida por el protocolo gráfico de Kitty,
que vive en una capa aparte gestionada por el terminal, posicionada mediante "unicode placeholders"
(ver nota ya existente en el proyecto: "ratatui-image v11 usa unicode placeholders, la imagen
desaparece sola al pisarse las celdas" — funcionaba bien para cambios de VISTA, pero parece que un
modal superpuesto Y CERRADO encima no dispara la misma limpieza).

**Pistas para investigar antes de aplicar un fix a ciegas:**
- Revisar si el cierre del modal fuerza un redraw completo del frame (full clear) o solo redibuja
  las celdas que cambiaron respecto al frame anterior — si es un diff parcial, podría estar dejando
  intacta la posición de la imagen antigua mientras el terminal ya renderizó algo nuevo ahí encima.
- El protocolo gráfico de Kitty tiene un comando explícito de "borrar todas las imágenes" (acción
  `a=d` en el escape sequence `_Ga=d_`) — evaluar si conviene emitirlo al cerrar cualquier modal que
  se haya superpuesto a un panel con imagen, forzando luego un re-render limpio de esos paneles.
- Alternativa más simple si lo anterior es complicado: forzar un `Clear` completo de terminal (no
  solo del área del modal) al cerrar CUALQUIER modal en Vista 2/3 específicamente, re-emitiendo la
  imagen del oscilador desde cero justo después — más brusco pero más robusto contra este tipo de
  artefacto.
- Reproducir el bug de forma controlada primero (abrir ayuda en Vista 2, cerrar, capturar con el
  driver pty + decodificador de transmisiones Kitty ya usado en el spike de renderizado, para ver
  exactamente qué queda mal) antes de aplicar cualquier fix — mismo rigor que se ha aplicado a
  otros bugs de este proyecto (diagnóstico con evidencia antes de tocar código).

## Ejecutar hyperT completo en el RedMagic (servidor remoto) — nota de arquitectura, futura
Alternativa considerada a "solo alojar datos de OI en el RedMagic": ejecutar la APP ENTERA en el
servidor y acceder remotamente desde la laptop, en vez de correrla siempre en local. Evaluado como
viable, sin bloqueante técnico real.

**Por qué es viable:** el binario es Rust puro, compilable/ejecutable en ARM vía Termux (mismo
patrón que ya usa el usuario para su wallet de BTC en Raspberry Pi/Termux). El renderizado de
hyperT ocurre íntegramente vía escape sequences de terminal (protocolo gráfico Kitty o fallback
Unicode) — no requiere GPU ni pantalla local, así que no importa que el proceso corra en un
teléfono sin monitor. WalletConnect no cambia nada: el QR se genera igual dentro del proceso, se ve
en el terminal remoto, se escanea con el móvil de MetaMask exactamente igual que en local.

**Matiz de seguridad a no olvidar:** si se hace esto, `secrets/agent_mainnet.json` pasa a vivir en
el RedMagic en vez de en la laptop — el teléfono se convierte en la máquina que controla dinero
real, así que su seguridad (acceso físico, bloqueo, etc.) importa tanto como la de la laptop ahora.

**Vías de acceso remoto evaluadas, de más a menos fiel visualmente:**
1. **`kitty +kitten ssh usuario@host`** — la opción recomendada. SSH normal NO reenvía el protocolo
   gráfico de Kitty por sí solo (caería a fallback halfblocks); el kitten de Kitty sí lo reenvía
   correctamente de extremo a extremo, dando la experiencia visual completa e idéntica a correrlo
   en local.
2. **SSH plano** — funciona sin más, cae al fallback halfblocks ya construido. Nada se rompe, solo
   se pierde el pulido visual de las imágenes rasterizadas.
3. **tmux/screen** — útil para persistencia de sesión (desconectar/reconectar sin perder estado),
   pero con soporte más delicado/parcial del protocolo gráfico Kitty — a probar si se prioriza la
   persistencia sobre el renderizado perfecto.
4. **mosh** — para conexiones inestables (ej. acceso desde móvil cambiando de red), prioriza
   resiliencia de conexión sobre renderizado, mismo matiz que tmux con el protocolo gráfico.
5. **Terminal web (ttyd o similar)** — acceso desde cualquier navegador sin configurar cliente,
   pero probablemente cae a fallback halfblocks (la mayoría de terminales web no implementan Kitty
   graphics protocol completo).

**Red de acceso — NO exponer el RedMagic a internet pública directamente**, coherente con el
perfil de privacidad del usuario (ya usa I2P para otra infraestructura personal). Opciones
evaluadas:
- **WireGuard/Tailscale** entre los propios dispositivos del usuario — recomendado para este caso
  concreto: acceso solo entre sus propias máquinas, sin exponer el puerto SSH al mundo, más simple
  y con menos latencia que I2P para este uso puramente personal.
- Reusar la infraestructura I2P ya existente del usuario, si prefiere consistencia con el resto de
  su setup — viable pero más complejo/latente que WireGuard para este caso.

**Recomendación consolidada (pendiente de que el usuario monte el RedMagic para ejecutarlo):**
`kitty +kitten ssh` sobre una red WireGuard entre laptop y RedMagic — da la experiencia visual
completa sin exponer nada a internet. No implementar nada de esto todavía — es solo la nota de
diseño para cuando el servidor físico exista y el usuario decida ejecutar la app ahí en vez de (o
además de) usarlo solo como fuente de backfill de datos (ver sección de histórico de OI/trades).

## Fuente externa opcional de histórico de OI y trades (futuro servidor RedMagic del usuario)
El usuario planea montar un dispositivo propio (fuera de este repo, sin detalles de infraestructura
personal aquí, aún pendiente de comprar el teléfono para montarlo) como servidor 24/7 para acumular
histórico de OI y de trades, y así evitar depender del uptime de sesión del propio TUI. Diseño
recomendado, cuando se construya:

**Qué SÍ se beneficia de esto** (dependen de acumulación en memoria, se resetea al cerrar la app):
- ΔOI 5m/1h (Ranking Vista 1, Heatmap top-OI Vista 4)
- Rotación ΔOI 1h/4h/24h (Vista de Flujo) — la ventana de 24h es la que más tarda en llenarse
- La señal descartada de densidad de liquidación por OI-delta (`src/liqdens.rs`, ver más arriba) —
  única señal que de verdad necesitaba ~61 velas de calendario real; si se monta este servidor,
  sería el momento de reconsiderar recuperarla
- **Delta por vela** (Vista 2) — mismo problema estructural que el OI — Hyperliquid no da
  histórico retroactivo del feed de trades clasificado por lado agresor, solo lo que se escucha en
  vivo desde que el WS se conecta. Temporalidades cortas (1m, 5m) se rellenan rápido; 4h y sobre
  todo 1d son tan poco realistas de llenar con solo la sesión del TUI como lo era el ΔOI 24h.
- **CVD y su divergencia** (Vista de Flujo) — mismo motivo: se resetea al cambiar de par y al
  cerrar la app, sin backfill posible vía API.

**Qué NO lo necesita** (ya funcionan sin esto, no tocar):
- Heatmap de liquidaciones estimado (Vista 5) — snapshot actual de OI×volumen×leverage, no historial
- Percentil de funding 30d (Vista de Flujo) — viene de `fundingHistory`, dato real de Hyperliquid ya
  paginado, no algo acumulado localmente
- Whale positioning (Vista 7) — leaderboard + clearinghouseState, sin dependencia de OI histórico

**Arquitectura de conexión (a construir cuando el servidor exista, no ahora):**
- El servidor corre una variante ligera del poller ya existente (`probe`) MÁS un listener del canal
  `trades` (igual que `ws_coin_trades`, pero corriendo 24/7 en el servidor en vez de solo durante la
  sesión del TUI), guardando snapshots de OI y buckets de delta (mismo esquema de 1 minuto que ya
  usa `DeltaState`) por par con timestamp, en almacenamiento simple (SQLite o JSON), expuestos vía
  un endpoint HTTP mínimo en red local del usuario.
- El TUI, si se le configura una URL (flag o variable de entorno, ej. `--oi-source` o
  `OI_SOURCE_URL`), hace un fetch de "backfill" al arrancar para rellenar tanto el historial de OI
  como los buckets de `DeltaState`/CVD en memoria de golpe, en vez de partir de cero.
- CRÍTICO: esto debe ser estrictamente opcional — si la URL no está configurada, o el servidor no
  responde, el TUI cae al comportamiento actual sin romperse (acumular en vivo, mostrar "—" mientras
  tanto). Nunca debe ser un requisito duro para que la app funcione, ya que la mayoría de quien use
  este proyecto (si se publica en GitHub) no tendrá ese servidor montado.
- No implementar nada de esto todavía — es una nota de diseño para cuando el usuario confirme que
  el servidor físico ya existe y está listo.

## Stack técnico
- Lenguaje: Rust
- TUI: ratatui (migrado a 0.30.2)
- Terminal de desarrollo/referencia del usuario: Kitty. Para publicar en GitHub, documentar en el
  README que el mejor renderizado visual requiere un terminal con protocolo de gráficos Kitty
  (Kitty, Ghostty, WezTerm) — en otros, la app cae a renderizado Unicode (funcional, menos pulido).
- Datos: hyperliquid_rust_sdk, pineado a master (rev aac75585)
- Wallet: WalletConnect v2 (transporte propio, ver histórico del proyecto) para cuenta maestra
  MetaMask, agent wallet (`LocalWallet`) para trading del día a día — ya operativo en testnet Y
  mainnet (validación real de mainnet pendiente de una primera orden pequeña).
- Journal (pendiente, prioridad baja): Obsidian con YAML frontmatter, sync vía Syncthing/I2P
- Backtesting (pendiente, prioridad baja): Python/ccxt/pandas (pipeline existente)
