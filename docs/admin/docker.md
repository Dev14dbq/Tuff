# Docker

`Dockerfile` в корне собирает `tuff` из исходников и кладёт его в
`gcr.io/distroless/cc-debian12:nonroot`: glibc, сертификаты, без оболочки и без Java. Процесс
идёт от пользователя 65532.

```bash
docker build -t tuff .
docker run -it --name tuff -p 25565:25565 -v tuff-data:/data tuff
```

- `/data` — папка сервера и рабочая папка: мир, `server.properties`, списки, логи, копии,
  `cache/`. Это том; без `-v` данные пропадут вместе с контейнером.
- Порт 25565. В `CMD` стоит `--bind 0.0.0.0`: `server-ip=127.0.0.1` из настроек по умолчанию
  внутри контейнера снаружи недоступен.
- Свои аргументы заменяют `CMD` целиком: `docker run ... tuff --bind 0.0.0.0 --offline`.
- `docker stop` шлёт SIGTERM: сервер сохраняет мир и выходит, как по `/stop`.
- Консоль: `docker attach tuff`, отключиться — `Ctrl+P Ctrl+Q`. Без `-it` консоль закрыта, команды
  идут через RCON или JSON-RPC.

## Данные игры

В образе нет файлов Mojang. Первый запуск сам скачивает `server.jar` у Mojang в
`/data/cache/mojang/` и достаёт данные в `/data/cache/mcdata/` (см.
[settings.md](settings.md)). Контейнеру нужен выход в интернет один раз.

Таблицы `tuff-reports-26.3.zip` образ, собранный из исходников, скачивает из релиза своей
версии. Чтобы обойтись без этого, положите файл из архива релиза в папку сервера:

```bash
docker run -it -p 25565:25565 \
  -v "$PWD/tuff-reports-26.3.zip:/data/tuff-reports-26.3.zip:ro" \
  -v tuff-data:/data tuff
```

Для разработки можно смонтировать готовые данные из `cargo xtask extract-data`:
`-v "$PWD/cache/mcdata:/data/cache/mcdata:ro"`.

## Папка с хоста вместо тома

Папка должна быть доступна на запись пользователю 65532, либо запускайте от своего:

```bash
docker run -it -p 25565:25565 --user "$(id -u):$(id -g)" -v "$PWD/server:/data" tuff
```
