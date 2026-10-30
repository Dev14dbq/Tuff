# Переход с ванили и с Paper/Spigot

Сначала копия всей папки сервера. Tuff в альфе.

## Версия мира

Tuff читает и пишет чанки и игроков в формате 26.3. Обновлять старый формат, как это делает
ванильный DataFixer, он не умеет. Мир с более старой версии сначала откройте в ванильном
сервере 26.3 (лучше с `--forceUpgrade`), остановите его и только потом переносите. Игроки
должны быть в `world/players/data/`, как пишет 26.3; старая папка `playerdata/` не читается.

## С ванили 26.3

1. Остановите ванильный сервер (`/stop`), чтобы мир сохранился.
2. Положите `tuff` в папку сервера или запустите его оттуда. Ничего переименовывать не нужно.
3. Проверьте `server-ip`: у Tuff по умолчанию `127.0.0.1`. Если в вашем `server.properties`
   стоит пусто, он так и останется, и сервер будет слушать все адреса.
4. Запустите `tuff`. Первый запуск скачает `server.jar` в `cache/` (см.
   [settings.md](settings.md)).

Что переносится как есть: мир (`region/`, `entities/`, `poi/`, `level.dat`, данные мира,
`datapacks/`), игроки (инвентарь, позиция, режим; остальное сохраняется в файле нетронутым),
достижения и статистика, `server.properties`, `ops.json`, `whitelist.json`,
`banned-players.json`, `banned-ips.json`, `usercache.json`, `server-icon.png`,
`server-links.json`, `codeofconduct.txt`.

Обратно на ваниль 26.3 можно в любой момент: Tuff пишет те же файлы. Свои ключи Tuff
(`backup-*`, `proxy`, ...) ваниль просто проигнорирует, `pregen.json` и `backups/` можно
удалить.

Что не переносится: `eula.txt` не нужен; ключи `server.properties` из списка «пока не
действуют» в [settings.md](settings.md) читаются, но не работают.

## С Paper или Spigot

Paper и Spigot держат Незер и Энд в отдельных папках рядом с миром:

```
world/            верхний мир, level.dat, игроки
world_nether/     DIM-1/region (или dimensions/minecraft/the_nether/region)
world_the_end/    DIM1/region (или dimensions/minecraft/the_end/region)
```

Tuff находит эти папки сам, если они лежат рядом с папкой из `level-name` и называются
`<имя>_nether` и `<имя>_the_end`. Чанки там же и остаются: новые и изменённые чанки Tuff
пишет обратно в папку, где нашёл файлы региона. Переносить ничего не нужно. `--forceUpgrade`
тоже проходит по этим папкам. Старые `DIM-1/` и `DIM1/` внутри `world/` тоже читаются.
Новое измерение без файлов получает путь 26.3: `world/dimensions/minecraft/<измерение>/`.

Что не переносится:

- плагины (`plugins/` и их данные): API плагинов пока нет;
- настройки `bukkit.yml`, `spigot.yml`, `paper-global.yml`, `paper-world-defaults.yml`,
  `commands.yml`, `permissions.yml`, `help.yml`: Tuff их не читает. Поведение — ванильное, со
  всеми механиками, которые Paper меняет или чинит (дюпы, TNT-машины, ферма железа и т. п.);
- права из плагинов вроде LuckPerms: есть только уровни операторов 0–4 из `ops.json`;
- `level.dat` в `world_nether/` и `world_the_end/` не нужен: правила, время и погода берутся из
  папки `world/`, как в ванили;
- прокси: Velocity modern forwarding и BungeeCord переносятся настройками в
  `server.properties` (`proxy`, `velocity-secret`, `bungee-tokens`), а не из
  `paper-global.yml`.

Если сервер Paper был не на 26.3, сначала обновите мир ванильным сервером 26.3, как сказано
выше. Ванильный сервер ждёт мир в ванильной раскладке: на время обновления перенесите
`world_nether/DIM-1` в `world/DIM-1` и `world_the_end/DIM1` в `world/DIM1`.
