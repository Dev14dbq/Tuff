# tuff

Сервер Minecraft Java Edition 26.3, написанный с нуля. Ванильный клиент 26.3 заходит на него
как на обычный выделенный сервер: тот же мир, `server.properties`, операторы из `ops.json`,
датапаки и команды.

Крейт сервера по-прежнему называется `mc-server`. Программа, которую он собирает, — `tuff`.

- [docs/ADMIN.md](docs/ADMIN.md) — запуск, права, списки, прокси, management, датапаки
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — как устроен сервер
- [docs/ROADMAP.md](docs/ROADMAP.md) — план по фазам
- [docs/CONCEPT.md](docs/CONCEPT.md) — сервер, клиент и платформа целиком

## Запуск

Нужны Rust (версия в `rust-toolchain.toml`) и извлечённые данные ванили, см. ниже.

```bash
cargo build --release -p mc-server
mkdir -p run && cd run && ../target/release/tuff --op <ник>
```

Сервер запускают в его папке. При первом старте там появляются `server.properties`,
`ops.json`, белый список, баны, `usercache.json`, `logs/latest.log` и мир `world/`.
`server-ip` по умолчанию `127.0.0.1`: слушает только этот компьютер. Пустое значение
открывает все адреса.

Мир ванильного сервера 26.3 кладётся как `world/`. Подробности и переход с Paper —
в [docs/ADMIN.md](docs/ADMIN.md).

```bash
RUST_LOG=debug ../target/release/tuff --op <ник>
```

`--offline` (или `online-mode=false`) выключает проверку аккаунтов. Если при этом
`enforce-secure-profile=true`, чат без подписи не рассылается: поставьте
`enforce-secure-profile=false`.

## Docker

В образ не входят данные игры и мир. Каталог данных монтируется снаружи.

```bash
docker build -t tuff .
docker run --rm -p 25565:25565 \
  -v "$PWD/cache/mcdata:/data/cache/mcdata:ro" \
  -v tuff-data:/data \
  tuff --bind 0.0.0.0
```

Образ слушает `0.0.0.0:25565`. Рабочая папка контейнера — `/data`.

## Структура

```
crates/
  mc-nbt        NBT
  mc-protocol   пакеты 26.3, кадры, сжатие
  mc-data       таблицы блоков и реестров из извлечённых данных
  mc-world      секции, палитры, карты высот, свет на диске
  mc-worldgen   шум, биомы, поверхность
  mc-features   фичи, структуры, датапак-оверлей генерации
  mc-light      свет
  mc-entity     сущности, физика, навигация
  mc-tick       фазы тика, жидкости, редстоун
  mc-storage    регионы Anvil
  mc-command    дерево команд
  mc-server     сеть, тик, команды, бинарник tuff
xtask/          cargo xtask: extract-data, bots, probe, parity
docs/           устройство, администрирование, сверки
```

`cache/`, `reference/`, `tools/` и `run/` в git не попадают. В `reference/` лежит
проприетарный код Mojang, его только читают. В репозиторий из него ничего не копируется.

## Разработка

```bash
# данные ванили из reference/<версия>/server.jar → cache/mcdata/<версия>/
cargo xtask extract-data

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Нагрузочный прогон против уже запущенного сервера с `--offline`:

```bash
cargo xtask bots --count 100 --seconds 30
```

CI (`.github/workflows/ci.yml`) гоняет fmt, clippy и тесты на Linux, macOS и Windows
и не даёт закоммитить `reference/`, `tools/`, `cache/`, `*.jar` или `*.class`.

## Лицензия

Код в этом репозитории — [GPL-3.0-or-later](LICENSE). Jar-файлы, декомпилированный код
и данные Mojang не наши и здесь не лежат.
