# Настройки

Всё задаётся в `server.properties` в папке сервера. Первый запуск пишет файл со всеми
ключами ванили 26.3 и их значениями по умолчанию; ключи, которых в файле нет, дописываются при
каждом запуске. Отдельного конфига Tuff нет: свои ключи Tuff лежат в том же файле.

Почти все ключи читаются один раз при запуске. На ходу меняются только те, что меняет
JSON-RPC (`motd`, `max-players`, `view-distance`, `white-list`, см. [ADMIN.md](../ADMIN.md)).

## Ключи, которые работают

| Ключ | По умолчанию | Что делает |
|---|---|---|
| `server-ip` | `127.0.0.1` | Адрес, который слушает сервер. **Отличие от ванили:** там пусто. Пустое значение — все адреса. `--bind` перекрывает на один запуск. |
| `server-port` | `25565` | Порт. `--port` перекрывает. |
| `motd` | `A Minecraft Server` | Описание в списке серверов. |
| `max-players` | `20` | Лимит игроков (не меньше 1). |
| `online-mode` | `true` | Проверка аккаунта у Mojang. `--offline` выключает на один запуск. |
| `enforce-secure-profile` | `true` | Чат без подписи не доходит до других. В offline-режиме нужен `false`, иначе чат молчит. |
| `white-list` | `true` | Пускать только белый список и операторов. |
| `op-permission-level` | `4` | Уровень, который получает новый оператор (0–4). |
| `view-distance` | `10` | Дальность прорисовки в чанках (2–32). |
| `simulation-distance` | `10` | Дальность, на которой тикают чанки (2–32). |
| `network-compression-threshold` | `256` | Порог сжатия пакетов в байтах; отрицательное — без сжатия. |
| `level-name` | `world` | Папка мира. `--world` перекрывает. |
| `level-seed` | пусто | Сид нового мира: число или текст. `--seed` перекрывает. |
| `level-type` | `minecraft:normal` | Тип нового мира; значение со словом `flat` даёт суперплоский. Существующий мир оставляет свой генератор. |
| `gamemode` | `survival` | Режим игры нового мира. |
| `difficulty` | `easy` | Сложность нового мира. |
| `enable-status` | `true` | Отвечать на запрос списка серверов. `false` — сервер как будто выключен. |
| `hide-online-players` | `false` | Не показывать ники в списке серверов. |
| `allow-flight` | `false` | `false` кикает за долгий полёт без права летать. |
| `rate-limit` | `0` | Потолок пакетов в секунду от игрока; 0 — без потолка. |
| `max-tick-time` | `60000` | Сторож тика, мс. Тик дольше — отчёт в `crash-reports/`. 0 или меньше выключает. |
| `enable-code-of-conduct` | `false` | Показывать `codeofconduct.txt` до входа в мир. |
| `bug-report-link` | пусто | Ссылка «сообщить об ошибке» в меню паузы, если её нет в `server-links.json`. |
| `resource-pack`, `resource-pack-sha1`, `resource-pack-id`, `resource-pack-prompt`, `require-resource-pack` | пусто / `false` | Ресурспак сервера, как в ванили. |
| `enable-rcon`, `rcon.port`, `rcon.password` | `false`, `25575`, пусто | RCON. Без пароля не включается, в лог пишется предупреждение. |
| `broadcast-rcon-to-ops` | `true` | Операторы видят, что делают через RCON. |
| `enable-query`, `query.port` | `false`, `25565` | GameSpy4 Query (UDP). |
| `management-server-enabled`, `management-server-host`, `management-server-port`, `management-server-secret`, `management-server-tls-enabled` | `false`, `localhost`, `0`, пусто, `true` | Сервер управления JSON-RPC, см. [ADMIN.md](../ADMIN.md). |
| `text-filtering-config`, `text-filtering-version` | пусто, `0` | Внешний фильтр текста для чата, табличек и книг (JSON как в ваниле). Версия 0 работает; версия 1 (Azure, вход по сертификату) разбирается, но запросы не шлются: в лог пишется предупреждение, текст идёт без фильтра. |
| `initial-enabled-packs`, `initial-disabled-packs` | `vanilla`, пусто | Паки нового мира; `DataPacks` и `enabled_features` пишутся в `level.dat` как в ваниле. |

## Ключи Tuff

Ванильный сервер их не знает и просто оставит в файле.

| Ключ | По умолчанию | Что делает |
|---|---|---|
| `backup-interval` | `0` | Минуты между автоматическими копиями мира. 0 — только по `/backup`. |
| `backup-keep` | `5` | Сколько копий держать в `backups/` (не меньше 1). |
| `proxy` | `none` | `velocity` или `bungee`: вход только через прокси. |
| `velocity-secret` | пусто | Секрет modern forwarding, тот же, что у Velocity. |
| `bungee-tokens` | пусто | Токены BungeeGuard через запятую. Пусто — любой, кто достучался до порта, может назваться прокси. |

## Ключи, которые пока не действуют

Они есть в файле, потому что их пишет ваниль, но Tuff их не читает. Поведение такое, как
написано; если оно расходится с ванилью, это ошибка, о которой мы знаем.

| Ключ | Что сейчас |
|---|---|
| `accepts-transfers` | Переходы с другого сервера (Transfer) принимаются всегда. |
| `spawn-protection` | Защиты спавна нет. |
| `hardcore` | Новый мир всегда не хардкорный. |
| `generate-structures` | Структуры генерируются всегда. |
| `generator-settings` | Не читается; суперплоский мир берёт слои из `world_gen_settings.dat` мира. |
| `force-gamemode` | Режим при входе не сбрасывается. |
| `enforce-whitelist` | После `/whitelist reload` и правок списка уже вошедших не кикает. |
| `player-idle-timeout` | Бездействующих не кикает (`/setidletimeout` отвечает, но таймер не работает). |
| `pause-when-empty-seconds` | Пустой сервер не ставит тик на паузу. |
| `region-file-compression` | Чанки пишутся в zlib, как при значении `deflate`. |
| `function-permission-level`, `max-world-size`, `max-chained-neighbor-updates`, `entity-broadcast-range-percentage`, `sync-chunk-writes`, `broadcast-console-to-ops`, `log-ips`, `prevent-proxy-connections`, `chat-spam-threshold-seconds`, `command-spam-threshold-seconds`, `status-heartbeat-interval`, `use-native-transport`, `enable-jmx-monitoring`, `management-server-allowed-origins`, `management-server-tls-keystore`, `management-server-tls-keystore-password` | Не читаются. |

## Аргументы командной строки

`tuff --help` печатает список. Ванильные: `--universe <папка>`, `--world <имя>`,
`--port <порт>`, `--initSettings` (записать `server.properties` и выйти), `--forceUpgrade`,
`--nogui` (окна и так нет). `--bonusChest`, `--eraseCache`, `--safeMode` принимаются и ничего не
делают.

Свои:

- `--bind <адрес>` — адрес на один запуск вместо `server-ip`;
- `--data <папка>` — готовые данные игры (по умолчанию `cache/mcdata/<версия>` в папке сервера,
  выше неё или рядом с бинарником);
- `--reports <zip>` — таблицы `tuff-reports-<версия>.zip` для первого запуска (по умолчанию
  рядом с `tuff` или в папке сервера, иначе скачиваются из релиза этой версии Tuff);
- `--seed <сид>`, `--op <ник>` (можно несколько раз), `--offline`;
- `--no-console` — не читать команды со стандартного ввода;
- `-V`, `--version` — версия Tuff, Minecraft и протокола.

Переменные окружения: `RUST_LOG` (уровень лога, например `RUST_LOG=debug`), `MC_THREADS`
(сколько потоков берёт генерация мира; по умолчанию все ядра).

## Данные игры при первом запуске

Tuff не содержит файлов Mojang. Если готовых данных нет, первый запуск:

1. Берёт таблицы `tuff-reports-26.3.zip` (номера блоков, реестров, пакетов, дерево команд,
   свойства блоков): из архива релиза рядом с `tuff`, из папки сервера, из `--reports` или
   скачивает из релиза на GitHub с проверкой SHA-256.
2. Находит 26.3 в манифесте версий Mojang (`piston-meta.mojang.com`), скачивает `server.jar` с
   `piston-data.mojang.com` (около 60 МБ) и проверяет SHA1. Каждый шаг пишется в лог с адресом.
3. Достаёт из jar ванильный датапак, `version.json` и английский текст для консоли.

Всё ложится в `cache/` папки сервера: `cache/mojang/26.3/` (jar и его описание) и
`cache/mcdata/26.3/`. Дальше запуск идёт без сети. Java не нужна. Удалить `cache/mcdata` —
данные соберутся заново из сохранённого jar.
