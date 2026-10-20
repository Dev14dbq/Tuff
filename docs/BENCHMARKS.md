# Базовые замеры

`cargo xtask bench` — ваниль 26.3 (`reference/26.3/server.jar`, `-Xmx2G`) и наш сервер
(`--release`), каждый сценарий с нового мира, сид −1294266004. Сценарии описаны в
`xtask/src/bench.rs`. MSPT — из `tick query` каждые 5 с за 60 с (медиана P50 и P99 окон по
100 тиков); RAM — footprint процесса в конце (macOS, `top`).

**Машина:** Apple M4 Pro (12 ядер), 24 ГБ, macOS 27.2, OpenJDK 25.0.4.1. **Дата:** 2026-10-20.
Параллельно работали другие сессии (load average 1.6–8.4) — цифры шумные, сравнивать порядки.

| scenario | server | result |
|---|---|---|
| gen10 | vanilla | 81 chunks/s (473 chunks in 5849 ms), RAM 760 MB |
| gen10 | ours | 138 chunks/s (441 chunks in 3192 ms), RAM 159 MB |
| gen32 | vanilla | 127 chunks/s (3725 chunks in 29230 ms), RAM 2389 MB |
| gen32 | ours | 198 chunks/s (4225 chunks in 21319 ms), RAM 384 MB |
| bots | vanilla | 50 bots: MSPT mean 12.1, p50 11.7, p99 18.4, RAM 1080 MB |
| bots | ours | 50 bots: MSPT mean 0.0, p50 0.0, p99 0.2, RAM 280 MB |
| idle | vanilla | empty field: MSPT mean 0.6, p50 0.6, p99 1.9, RAM 584 MB |
| idle | ours | empty field: MSPT mean 0.0, p50 0.0, p99 0.5, RAM 104 MB |
| hoppers | vanilla | 16384 hoppers: MSPT mean 6.6, p50 6.7, p99 25.4, RAM 643 MB |
| hoppers | ours | 16384 hoppers: MSPT mean 0.1, p50 0.0, p99 0.5, RAM 113 MB |
| redstone | vanilla | 2048 observer clocks + 4096 dust: MSPT mean 5.5, p50 3.9, p99 26.2, RAM 559 MB |
| redstone | ours | 2048 observer clocks + 4096 dust: MSPT mean 14.5, p50 24.0, p99 29.6, RAM 36 MB |
| entities | vanilla | 2000 cows: MSPT mean 14.1, p50 14.1, p99 17.5, RAM 529 MB |
| entities | ours | 2000 cows: MSPT mean 0.0, p50 0.0, p99 0.1, RAM 104 MB |

**Как читать.** Наш сервер пока не тикает хопперы, ИИ мобов и редстоун так, как ваниль
(Ф3/Ф5), поэтому его MSPT в `bots`, `hoppers` и `entities` — не сравнение, а нижняя граница.
`redstone` у нас дороже из-за обновлений пыли без настоящей логики. Ваниль в `gen10` шлёт
473 чанка (кольцо сверх 21×21), в `gen32` — 3725 из 4225 к моменту тишины 30 с.

Повторить: `cargo xtask bench [--only vanilla|ours] [--scenarios gen10,bots,...]`.

⏳ Lithium/C2ME и профиль `/jfr` — не снимались (нужно согласие владельца на загрузки).
