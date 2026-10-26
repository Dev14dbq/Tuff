# Базовые замеры

`cargo xtask bench` — три стороны: ваниль 26.3 (`reference/26.3/server.jar`), ваниль под
Fabric с Lithium и C2ME (`modded`, jar'ы в `tools/fabric/26.3`) и наш сервер (`--release`).
Каждый сценарий с нового мира, сид −1294266004. Сценарии описаны в `xtask/src/bench.rs`.
MSPT — из `tick query` каждые 5 с за 60 с (медиана P50 и P99 окон по 100 тиков); RAM —
footprint процесса в конце (macOS, `top`).

## Как мерить честно

На macOS нельзя отдать процессу отдельные ядра (нет cpuset/isolcpus), поэтому bench не
изолирует сервер, а добивается тихой машины и проверяет её (`xtask/src/quiet.rs`):

- **Одинаковые ресурсы.** Каждый сервер получает `--cores` потоков (по умолчанию 4 —
  помещаются на 8 P-ядер M4 Pro): JVM `-XX:ActiveProcessorCount`, наш `MC_THREADS` и
  `TOKIO_WORKER_THREADS`; куча Java — 2 ГБ.
- **Тихая машина перед каждым прогоном.** Ждём, пока нет клиента Minecraft, питание от сети
  и остальные процессы за последние 10 с в среднем заняли не больше `--background` ядер
  (по умолчанию 0,5; считается по `top`, без самого bench и его сервера). Mac не засыпает
  (`caffeinate`). Не дождались за 15 мин — bench падает с причиной.
- **Фон во время прогона.** Сторож замеряет фон каждую секунду; если в среднем он выше
  лимита — прогон повторяется (до двух раз), иначе строка помечается ⚠ noisy. В таблице —
  средний фон и пик.
- **Раунды вперемешку.** `--rounds 3`: каждый сценарий трижды, стороны чередуются в разном
  порядке; в ячейке медиана и [мин–макс].
- **Профиль отдельно.** `--jfr` добавляет ещё один раунд для Java-сторон под `/jfr`; его цифры
  в таблицу не идут (профайлер сам ест время).

Перед замером: закрыть игру, браузер и всё тяжёлое, подключить питание. `--noisy` снимает
проверки (только для отладки, цифры не публикуются).

Повторить: `cargo xtask bench [--only vanilla,modded,ours] [--scenarios gen10,bots,...]
[--rounds 3] [--cores 4] [--jfr]`.

**Настоящая изоляция** — только на отдельной Linux-машине: ядра под сервер через cpuset,
без турбобуста и чужих процессов. Пока такой машины нет.

## Предварительная таблица (2026-10-20, без проверок)

**Машина:** Apple M4 Pro (12 ядер), 24 ГБ, macOS 27.2, OpenJDK 25.0.4.1. Снята до сторожа:
параллельно работали другие сессии (load average 1.6–8.4), потоки не ограничены, по одному
прогону. Цифры шумные, сравнивать только порядки. Сторону `modded` пока не снимали чисто.

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

## Профиль ванили (`/jfr`, 2026-10-26)

Сэмплы потока сервера (тик) за 60 с измеряемого окна; «верхний кадр» — первый метод
`net.minecraft` в стеке (листья JDK и fastutil засчитаны вызывающему). Сняты до сторожа, при
фоновой нагрузке: абсолютное время шумное, доли — нет. Записи: `cache/jfr/<scenario>-vanilla.jfr`,
сводка: `cargo xtask bench --summarize <file>`.

| сценарий | что съедает тик (верхний кадр, % сэмплов) |
|---|---|
| hoppers | `PersistentEntitySectionManager.areEntitiesLoaded` 21.7, `SavedDataStorage.get` 13.0, `Level.updateNeighbourForOutputSignal` 9.3, обёртка тика блок-сущности 8.0, `SimulationChunkTracker.getLevel` 6.4, `ChunkMap.getVisibleChunkIfPresent` 5.8 |
| redstone | `LevelChunkTicks.poll` 13.2, `Level.getChunk` 12.2, `StateHolder.valueIndex` 8.6, `ServerChunkCache.getChunk` 6.5, `getSignal` 6.3, `updateShape` 5.3 |
| entities | `BlockCollisions.computeNext` 26.4, `EntitySection.getEntities` 13.7, `Level.getChunk` 8.6, `ServerChunkCache.getChunk` 4.0 |
| bots | `ChunkMap$TrackedEntity.getEffectiveRange` 16.1, `forEachAccessibleNonEmptySection` 5.1, `FluidState.isRandomlyTicking` 4.4, `PalettedContainer.get` 4.0 |

Вывод для Ф7: у хопперов треть тика — проверки «загружен ли чанк / сущности» на каждый
хоппер, а не перенос предметов; у редстоуна и коров — поиск чанка по координатам и
столкновения. Это первые цели для кешей.
