# Команды

## Ванильные

Дерево команд — то же, что выделенный сервер ванили 26.3 отправляет оператору: те же
подсказки, уровни прав и сообщения через ванильные ключи перевода. Консоль и RCON работают с
уровнем 4, командный блок — с уровнем 2.

Все 92 корневые команды выделенного сервера на месте:

`advancement`, `attribute`, `ban`, `ban-ip`, `banlist`, `bossbar`, `clear`, `clone`, `damage`,
`data`, `datapack`, `debug`, `defaultgamemode`, `deop`, `dialog`, `difficulty`, `effect`,
`enchant`, `execute`, `experience`/`xp`, `fetchprofile`, `fill`, `fillbiome`, `forceload`,
`function`, `gamemode`, `gamerule`, `give`, `help`, `item`, `jfr`, `kick`, `kill`, `list`,
`locate`, `loot`, `me`, `msg`/`tell`/`w`, `op`, `pardon`, `pardon-ip`, `particle`, `perf`,
`place`, `playsound`, `posteffect`, `random`, `recipe`, `reload`, `return`, `ride`, `rotate`,
`save-all`, `save-off`, `save-on`, `say`, `schedule`, `scoreboard`, `seed`, `setblock`,
`setidletimeout`, `setworldspawn`, `spawnpoint`, `spectate`, `spreadplayers`, `stop`,
`stopsound`, `stopwatch`, `summon`, `swing`, `tag`, `team`, `teammsg`/`tm`, `teleport`/`tp`,
`tellraw`, `test`, `tick`, `time`, `title`, `transfer`, `trigger`, `version`, `waypoint`,
`weather`, `whitelist`, `worldborder`.

Где поведение пока расходится с ванилью:

- `/perf start|stop` пишет отчёт `debug/profiling/<дата>-world.txt`: число тиков, MSPT p50 и
  p99, время по фазам (блоки, сущности, редстоун, блочные сущности, остальное). У ванили там
  zip-архив профайлера.
- `/debug start|stop` считает тики и TPS, файл отчёта не пишет.
- `/jfr start|stop` отвечает как ваниль, но записи нет: JFR — часть Java.
- `/test`: запускателя GameTest нет, существующие тесты (`always_pass`) проходят сразу.
- `/setidletimeout` отвечает, но бездействующих не кикает.

## Команды Tuff

### `/backup` (уровень 4)

Копия папки мира без остановки сервера, в `backups/<время в мс>/` рядом с
`server.properties`. `session.lock` не копируется. Копия идёт в отдельном потоке; пока одна
не кончилась, вторая не начнётся. Держится `backup-keep` последних копий, старые удаляются.
Автоматически — каждые `backup-interval` минут.

Команда не сохраняет мир перед копией: сначала `/save-all`, если нужно самое свежее.

### `/pregen` (уровень 2)

Генерация чанков заранее.

```
/pregen start <x> <z> <радиус> [square|circle] [измерение]
/pregen pause | continue | cancel | status
```

- `<x> <z>` — центр в блоках, `<радиус>` — в блоках, от 0 до 50 000. Чанк-центр —
  `x / 16`; радиус 16 даёт квадрат 3×3 чанка.
- `square` (по умолчанию) — все чанки в квадрате, `circle` — чанки, чей центр в круге.
- Измерение: `overworld`, `nether`, `end` (или полные `minecraft:the_nether` и т. п.).
  По умолчанию — то, где стоит игрок; из консоли — верхний мир.
- Один чанк за тик: сгенерировать, записать на диск, выгрузить. Большой радиус занимает
  время и чуть удлиняет тики.
- Задача хранится в `pregen.json` в папке сервера и продолжается после рестарта.
- Прогресс пишется в лог и тому, кто запустил, каждые 256 чанков и в конце.

### `/chase` (уровень 2)

Отладочная команда ванили, которой нет в выделенном сервере. Один игрок ведёт, другие на
других серверах повторяют его положение.

- `/chase lead [адрес] [порт]` — слушать TCP (по умолчанию `localhost:10000`) и каждому
  подключившемуся слать строку `execute in <измерение> run tp @s <x> <y> <z> <yaw> <pitch>`
  при каждом движении.
- `/chase follow [адрес] [порт]` — подключиться к ведущему и телепортироваться за ним.
- `/chase stop` — закрыть своё.

## Консоль

Команды набираются в терминале, где запущен `tuff` (без `/`). `--no-console` выключает
чтение. В Docker консоль доступна через `docker attach`, если контейнер запущен с `-it`.
