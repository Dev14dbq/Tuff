# minecraft-rewrite

Рабочая папка для изучения логики Minecraft Java Edition и написания своей реализации.

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — архитектура: слабые места ваниллы и наши решения, тик, моды
- [docs/ROADMAP.md](docs/ROADMAP.md) — план работ по фазам с критериями готовности
- [docs/specs/protocol-26.3.md](docs/specs/protocol-26.3.md) — где протокол 26.3 расходится с документацией и как это проверять
- [docs/specs/world-26.3.md](docs/specs/world-26.3.md) — формат мира 26.3 и сверка чанков с ванилью

## Референс

- **Версия:** Minecraft Java Edition **26.3** (latest release, вышла 2026-09-15), без обфускации, маппинги не нужны.
- **Откуда:** официальный манифест Mojang
  `https://piston-meta.mojang.com/mc/game/version_manifest_v2.json` → `latest.release` →
  `https://piston-meta.mojang.com/v1/packages/11f1aefe473eefba0af62e3286c33b692807b2eb/26.3.json`
- **client.jar:** `https://piston-data.mojang.com/v1/objects/e877b6a07acd633fb3bb475002175cec036e7b87/client.jar`
  (SHA1 `e877b6a07acd633fb3bb475002175cec036e7b87`, сверен)
- **server.jar:** `https://piston-data.mojang.com/v1/objects/33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c/server.jar`
  (SHA1 `33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`, сверен). Это бандлер: сам сервер лежит внутри,
  в `META-INF/versions/26.3/server-26.3.jar` (SHA256 из `META-INF/versions.list` сверен).
- **Декомпилятор:** [Vineflower](https://github.com/Vineflower/vineflower/releases) 1.12.0 → `tools/vineflower.jar`.
- **Java:** нужна 21+ (у 26.3 в манифесте указано `javaVersion: 25`).

Всё перечисленное лежит в `reference/` и `tools/`. Обе папки в `.gitignore` и в репозиторий не попадают.

## Декомпиляция

```bash
# клиент
java -jar tools/vineflower.jar reference/26.3/client.jar reference/26.3/client-src/

# сервер: сначала достать внутренний jar из бандлера
unzip -p reference/26.3/server.jar META-INF/versions/26.3/server-26.3.jar > reference/26.3/server-26.3.jar
java -jar tools/vineflower.jar reference/26.3/server-26.3.jar reference/26.3/server-src/
```

## Разработка

Нужны Rust (версия зафиксирована в `rust-toolchain.toml`, rustup поставит её сам) и Java 25+.

```bash
# данные ванили из reference/<версия>/server.jar → cache/mcdata/<версия>/
cargo xtask extract-data

# то же, что проверяет CI
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Запуск сервера

```bash
cargo run --release -p mc-server
```

Сервер слушает `127.0.0.1:25565` в **online-режиме**, как ванильный: аккаунт проверяется через
сервер сессий Mojang, соединение шифруется, скины работают. В клиенте Minecraft 26.3: «Сетевая
игра» → «Прямое подключение» → `localhost`. Чтобы пустить других, добавь `--bind 0.0.0.0`. Мир
плоский, режим творческий. Опции: `--help`, подробные логи: `RUST_LOG=debug`.

`--world <папка>` раздаёт мир, сохранённый ванильным сервером 26.3, вместо плоского. Пока только
для чтения, см. [docs/specs/world-26.3.md](docs/specs/world-26.3.md).

`--offline` отключает проверку аккаунтов и шифрование, тогда войти можно под любым именем. Это
нужно для ботов и тестов:

```bash
cargo run --release -p mc-server -- --offline
cargo xtask bots --count 100 --seconds 30   # нагрузочный тест
cargo xtask probe --mover                   # сверка форматов, см. docs/specs/protocol-26.3.md
cargo xtask registry-diff --a <ваниль> --b <наш>   # реестры в NBT против ванили
```

`extract-data` сверяет SHA1 server.jar с JSON версии, достаёт встроенный датапак и `version.json`
и запускает встроенный генератор данных Mojang (`--reports`): блоки и их состояния, реестры,
пакеты протокола, команды. Результат в `cache/` и в репозиторий не попадает.

CI (`.github/workflows/ci.yml`) заработает после пуша на GitHub. Кроме fmt/clippy/тестов на Linux
и macOS он падает, если в git попал `reference/`, `tools/`, `cache/`, `*.jar` или `*.class`.

## Лицензия

Наш код — [GPL-3.0-or-later](LICENSE). Это касается только кода в этом репозитории. Jar-файлы,
декомпилированный код и данные Mojang (`reference/`, `cache/`) не наши, не распространяются
и в репозиторий не попадают.

## Правило проекта: clean-room

Декомпилированный код Mojang проприетарный. Его можно **только читать**, чтобы понять логику
(алгоритмы, форматы данных, протокол, порядок тиков и т.п.).

- В код проекта ничего из `reference/` **не копируем**: ни файлы, ни классы, ни фрагменты.
- Пишем свою реализацию своими словами, по своему пониманию поведения.
- `reference/` и `tools/` никогда не коммитим.
