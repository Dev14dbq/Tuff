# Администрирование

Сервер заменяет ванильный выделенный сервер Java 26.3: те же папка мира, `server.properties`,
`ops.json`, белый список и баны. Клиент 26.3 заходит без модов.

Подробнее: [настройки](admin/settings.md), [команды](admin/commands.md),
[переход с ванили и Paper](admin/migration.md), [Docker](admin/docker.md).

## Запуск

Скачайте архив для своей системы со страницы релизов, сверьте с `SHA256SUMS`, распакуйте и
запустите `tuff` в папке сервера:

```bash
cd my-server && /путь/к/tuff --op <ник>
```

Java и Rust не нужны. Первый запуск скачивает `server.jar` у Mojang в `cache/` и достаёт из
него данные игры; таблицы `tuff-reports-26.3.zip` лежат в архиве рядом с `tuff`. Подробно — в
[settings.md](admin/settings.md#данные-игры-при-первом-запуске). Из исходников:
`cargo build --release -p mc-server`, см. CONTRIBUTING.md.

Первый запуск создаёт `server.properties` со значениями ванили 26.3, списки игроков, лог
`logs/latest.log` и мир `world/`. `server-ip` по умолчанию `127.0.0.1`: слушает только этот
компьютер. Пустое значение открывает все адреса. `tuff --version` печатает версию.

Остановка: `/stop`, Ctrl+C или SIGTERM (`kill`, systemd, `docker stop`); мир сохраняется.

Мир ванили 26.3 кладётся как `world/`. Измерения Paper (`world_nether`, `world_the_end`) и
старые `DIM-1` / `DIM1` читаются. `--forceUpgrade` переписывает чанки, которые читаются, в
текущий формат хранения и останавливается. Запускать его при остановленном сервере, один раз.

## Сборки и проверки

`.github/workflows/ci.yml` — fmt, clippy и тесты на Linux, macOS и Windows.
`supply-chain.yml` — `cargo deny` (лицензии, уязвимости, источники только crates.io,
`deny.toml`) и `cargo audit` на каждый push и раз в неделю. `release.yml` по тегу `v*`
собирает `tuff` для Linux (x86_64 и aarch64, glibc и статический musl), macOS (aarch64 и
x86_64) и Windows (x86_64), прикладывает SHA-256 и таблицы `tuff-reports-<версия>.zip` и
создаёт черновик релиза. Таблицы делает CI: генератор отчётов ванили и наблюдение за
ванильным сервером, для этого владелец ставит переменную репозитория
`MOJANG_EULA_ACCEPTED=true`. Список изменений — `CHANGELOG.md`.

## Права

Оператор — запись в `ops.json`. Уровень от 0 до 4, как в ванили. Новый оператор получает
`op-permission-level` (по умолчанию 4). Консоль и RCON идут с уровнем 4. Командный блок — с
уровнем 2. Отдельных групп и узлов прав нет.

## Списки

`whitelist.json`, `banned-players.json`, `banned-ips.json`, `usercache.json` лежат рядом с
`server.properties`. Команды те же: `/whitelist`, `/ban`, `/ban-ip`, `/pardon`, `/pardon-ip`,
`/op`, `/deop`. Старая запись списком из одного ника не выбрасывается: если ник есть в
`usercache.json`, она становится UUID, иначе остаётся по нику.

`white-list=true` пускает только список и операторов.

## Вход

`online-mode=true` проверяет аккаунт у Mojang. `online-mode=false` или флаг `--offline` пускает
любое имя и пишет в лог, что режим небезопасный.

`enforce-secure-profile=true` (ванильное значение) не отдаёт в чат сообщение без подписи и не
выкидывает игрока. В offline-режиме у клиента подписи нет, поэтому чат молчит, пока в
`server.properties` не стоит `enforce-secure-profile=false`.

`enable-code-of-conduct=true` показывает текст из `codeofconduct.txt` до входа в мир. Пустого
файла нет: в лог пишется предупреждение и вход идёт дальше.

`server-links.json` — массив ссылок в меню паузы. Свойство `bug-report-link` добавляет
встроенную ссылку «сообщить об ошибке», если в файле её ещё нет. Встроенная подпись в файле:

```json
[{ "builtin": "website", "url": "https://example.com" }]
```

Своя подпись: `{ "label": "Карта", "url": "https://example.com/map" }`. Имена `builtin`:
`bug_report`, `community_guidelines`, `support`, `status`, `feedback`, `community`, `website`,
`forums`, `news`, `announcements`.

Ресурспак задаётся ключами `resource-pack`, `resource-pack-sha1`, `resource-pack-id`,
`resource-pack-prompt`, `require-resource-pack`. Картинка списка серверов — `server-icon.png`
64×64.

`allow-flight=false` кикает за долгий полёт. `rate-limit` — потолок пакетов в секунду, 0
выключает. `max-tick-time` — сторож зависшего тика, отчёт в `crash-reports/`. Паника тоже пишет
отчёт туда.

## Прокси

`proxy=none` оставляет прямой вход. `proxy=velocity` требует тот же `velocity-secret`, что у
Velocity в modern forwarding. `proxy=bungee` ждёт `ip_forward: true`. `bungee-tokens` включает
BungeeGuard. Пустой список токенов означает, что любой, кто достучался до порта, может
представиться прокси. При включённом прокси прямой клиент не входит.

## Управление по JSON-RPC

`management-server-enabled=true`, непустой `management-server-secret` и порт. Протокол — одна
JSON-строка на сообщение по TCP, не WebSocket. `management-server-tls-enabled=true` на
`localhost` всё равно говорит открытым текстом и слушает только петлю. Адрес не с петли при
включённом TLS не открывается.

Первый вызов: `minecraft:server/authenticate` с `{"secret":"..."}`. Дальше списки, баны, баны
по IP, операторы, правила, motd, белый список, лимит игроков, кик, сохранение, остановка,
системное сообщение. Секрет в лог не пишется.

## Датапаки

Папка `world/datapacks`. `/reload` подхватывает функции, рецепты, лут и генерацию из включённых
паков: файл датапака заменяет ванильный с тем же путём. Пак, которому нужен выключенный флаг
(например `minecraft:trade_rebalance`), не включается, причина пишется в лог.

## Команды

Все команды выделенного сервера ванили и свои `/backup`, `/pregen`, `/chase` — в
[commands.md](admin/commands.md).

## Переход с ванили или Paper

Остановить старый сервер, запустить `tuff` в той же папке, проверить `server-ip`. Мир должен
быть уже в формате 26.3. Что переносится и что нет: [migration.md](admin/migration.md).
