# R00 · База, стенд и исходные измерения

Срез от 14 ноября 2026. Ветка `codex/r00-baseline` от `origin/dev`
`5a4a124de2099e065fd5b2d15cae1a6f4ef37e9f` (merge плана, PR #1). Исходный
снимок клиента — `3500e961ca5e9f05ec2b03e89084351a5902c8ac`.

## Окружение

| Параметр | Значение |
| --- | --- |
| Windows | Windows 11 Pro 10.0.26300, сборка 26300 |
| GPU / драйвер | NVIDIA GeForce RTX 4070 Ti, `32.0.16.1692` |
| rustc | `1.100.0 (0ef1bc386 2026-11-09)` |
| cargo | `1.100.0 (3acc79c0c 2026-11-05)` |
| host target | `x86_64-pc-windows-msvc` |
| Go (для эталона) | `go1.26.0 windows/amd64` |
| `.cargo/config.toml` | `target-feature=+crt-static` для `x86_64-pc-windows-msvc` |
| `Cargo.lock` | без изменений, исходный |
| Медиа | по умолчанию включено, `libwebrtc 0.4.0` |

Длинных путей, ломающих `libwebrtc`, на этой машине нет: `cargo check` и
`cargo test` используют `target/` в корне репозитория. Отдельный короткий
`CARGO_TARGET_DIR` не понадобился.

## Команды и результаты

Проверки выполнялись по отдельности, на ветке `codex/r00-baseline`.

| Команда | Результат |
| --- | --- |
| `cargo fmt --all -- --check` до правок | **код 1**: долг форматирования в 12 файлах (`src/voice_panel.rs`, `src/main.rs`, `src/attachments.rs`, `crates/voxhold-media/src/{devices,lib,runtime,stream}.rs` и панели) |
| `cargo fmt --all` | исправлено, отдельный коммит `R00: fix baseline rustfmt debt` (`96dc274`) |
| `cargo fmt --all -- --check` после | код 0 |
| `cargo check --workspace --locked` | успешно, 2m28s; предупреждения: неиспользуемые `px` и `Attachments::revision`, `VoicePanel::set_devices` |
| `cargo test --workspace --locked` | успешно: `voxhold-api` 4/4, `voxhold-media` 6/6, bin 0 тестов |
| `cargo build --release --locked -p voxhold-native-rust` | успешно, EXE 47 953 920 Б, GUI-subsystem |
| ручной запуск release-сборки | окно открывается, остаётся отзывчивым, паники нет |
| GitHub Actions `windows.yml` | workflow обновлён на `cargo check --workspace --locked` и `cargo test --workspace --locked`; результат прогона на GitHub в этом срезе не снят |
| работа с настоящим backend | **не выполнена**, см. «Стенд» |
| `--no-default-features` | не проверялось |

Предупреждения сборки относятся к исходному коду и не являются новыми
ошибками. Форматный долг закрыт отдельным коммитом, чтобы `R01` не смешивал
его со своими изменениями.

## Источники и ревизии

| Источник | Фактическое состояние на машине |
| --- | --- |
| Go | рабочее дерево на `09c82b983d5d842af0627289655b11c37ae56e1d`; `out/w01/Voxhold-w01-6e5db93.zip`, SHA-256 `F296A455D35DB2EC767C569DD45AB7B5DD6385E84D94041B53050BC5C8CE41E9` (совпадает с планом) |
| Go `bin/Voxhold.exe` | `go version -m` сообщает `09c82b983d5d+dirty`, поэтому для сравнения кадров нужен ZIP или чистая сборка; SHA-256 `19EB1756FF567BDF2F4FE5FBA4EECD7577191A830BD5EF826B62105DBD49F39D`, размер 24 081 408 Б |
| Backend | нужный коммит `9b29fdd6d5dc818b7408e4892b8e68b27d123831` присутствует в объектной базе; рабочее дерево стоит на `7f2a418f`, поэтому стенд нужно поднимать из `git archive` этого SHA |
| Frontend | закреплённый `ed7cb00ae12398753a8ef48a5a33b864e20cb04c` присутствует; рабочее дерево на `ed48d171` |
| `migrate` | доступен (`dev`), `sqlite3` CLI отсутствует; Docker 29.7.2 установлен |

## Стенд

Каталог стенда — вне репозитория:
`C:\Users\pnik2\AppData\Local\Temp\opencode\voxhold-qa` (создаётся при первом
прогоне), внутри `ref`, `server`, `fixtures`, `captures`, `perf`.

Команды для воспроизведения (PowerShell, без учётных данных в артефактах):

```powershell
$qa = 'C:\Users\pnik2\AppData\Local\Temp\opencode\voxhold-qa'
$backend = 'C:\Users\pnik2\OneDrive\Desktop\go\Voxhold-backend'
git -C $backend archive 9b29fdd6d5dc818b7408e4892b8e68b27d123831 | tar -x -C (Join-Path $qa 'server')
$env:DATABASE_PATH = Join-Path $qa 'server\.qa-data\voxhold-qa.sqlite'
$env:MIGRATIONS_PATH = Join-Path $qa 'server\migrations'
$env:ATTACHMENT_DIRECTORY = Join-Path $qa 'server\.qa-data\attachments'
$env:RECORDING_DIRECTORY = Join-Path $qa 'server\.qa-data\recordings'
$env:HTTP_LISTEN_ADDRESS = '127.0.0.1'
$env:HTTP_PORT = '18088'
$env:BOOTSTRAP_USERNAME = 'native_owner'
# BOOTSTRAP_PASSWORD вводится только в этом процессе
migrate -path $env:MIGRATIONS_PATH -database ('sqlite://' + $env:DATABASE_PATH.Replace('\','/')) up
go run ./cmd/bootstrap
go run ./cmd/api
```

Статус: команды подготовлены, но **не прогнаны**. `R00` выполнялся
одновременно с первым UI-срезом `R01`. По приёмке `R00` не закрыты:
изолированный backend и `/readyz`, seed, эталонные пары кадров,
release-прогон на этом дереве и первый замер памяти. Это не отменяет сборку и
тесты выше; статус зафиксирован таблицей, а не только разделом «Стенд».

## Сиды

Требуемый набор (аккаунты owner/admin/member, два сервера, text/news/voice,
200+ сообщений, ветка, ЛС, записи с превью и без) **не создан**. Сохранившиеся
каталоги `Voxhold-wgpu-native\out\w00\seed` и
`out\smoke\backend\tools\w01seed` нужно проверить на совместимость с
`9b29fdd` перед использованием.

## Матрица функций

См. [parity-matrix.md](parity-matrix.md). Строки отражают только проверенное в
этом срезе; работа пункта помечается «в работе», а не «готово».

## Ограничения этого прогона

- Кадры Go/Rust не сняты в `R00`: сравнение экранов выполнялось в `R01` на
  одном ПК, но без фиксации эталонной пары для каждого состояния.
- Замеры памяти не проводились.
- Сетевые сценарии не проверялись, потому что стенд не поднят.

## Следующий шаг

1. Поднять backend из `git archive 9b29fdd`, выполнить миграции и bootstrap,
   записать `/readyz`.
2. Создать воспроизводимый seed и снять исходные пары кадров Go/Rust для
   `auth-instance`, `auth-login`, `workspace-empty`, `workspace-text`.
3. Продолжить `R01`: тема, вход, раскладка и сплиттеры (ветка
   `codex/r01-ui-shell`).
