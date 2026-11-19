# Обновления VoxHold

Слой обновлений разделён на три части:

| Компонент | Назначение |
| --- | --- |
| `crates/voxhold-update` | библиотека: манифест, подпись, детерминированный ZIP, дельта, загрузка, staging, применение и откат |
| `crates/voxhold-updater` | маленький helper без GPUI: ждёт завершения клиента и применяет подготовленное обновление |
| `crates/voxhold-release` | CLI для ключей, упаковки, дельт, манифеста и проверки |

UI (иконка, прогресс, отмена, tooltip) в этот этап не входит: интеграция выполняется
после merge `W09g`. Библиотека не зависит от GPUI и может тестироваться отдельно.

## Канал и адреса

Один канал беты в закреплённом репозитории:

```text
https://github.com/LoonEman1/voxhold-native-rust/releases/download/beta/latest.json
https://github.com/LoonEman1/voxhold-native-rust/releases/download/beta/latest.json.sig
```

Ассеты релиза `beta`: `VoxHold-portable.zip`, `VoxHold-Setup.exe`,
`VoxHold-delta-<from_version>.zip`, `latest.json.sig`, `latest.json`.
Токены не используются и не должны требоваться. Разрешён только HTTPS;
исключение — HTTP на `127.0.0.1`, `::1` или `localhost` для локальной приёмки.
URL с query-параметрами разрешены, но в логи и ошибки попадает только
схема/хост/путь (`error::redact_url`).

`latest.json` загружается последним, чтобы клиент не увидел версию с ещё не
загруженными пакетами.

## Схема `latest.json` (schema 1)

```json
{
  "schema": 1,
  "channel": "beta",
  "version": "0.2.0",
  "min_windows": "10.0.19041",
  "published_at": "2026-11-19T12:00:00Z",
  "notes": "Что изменилось",
  "authenticode": true,
  "full": {
    "url": "https://github.com/.../beta/VoxHold-portable.zip",
    "size": 12345678,
    "sha256": "<sha256 zip>",
    "package_sha256": "<sha256 zip>"
  },
  "setup": {
    "url": "https://github.com/.../beta/VoxHold-Setup.exe",
    "size": 2345678,
    "sha256": "<sha256 setup>"
  },
  "deltas": [
    {
      "from_version": "0.1.0",
      "from_package_sha256": "<sha256 base zip>",
      "url": "https://github.com/.../beta/VoxHold-delta-0.1.0.zip",
      "size": 345678,
      "sha256": "<sha256 delta zip>"
    }
  ]
}
```

Проверки:

* `schema == 1`, `channel` совпадает с ожидаемым, `version` — semver;
* `min_windows` — `major.minor.build`; текущая сборка читается из реестра
  (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`);
* `published_at` — RFC 3339 UTC;
* `notes` не больше 64 КиБ, дельт не больше 32, размеры не больше 2 ГиБ;
* `sha256`/`package_sha256` — 64 символа нижнего регистра hex;
* URL: HTTPS, либо HTTP только для loopback; логин/пароль в URL запрещены;
* неизвестные поля JSON отклоняются;
* понижение версии — ошибка `Downgrade`, равная версия — `UpToDate`;
* дельта выбирается только при совпадении `from_version` и
  `from_package_sha256` и только если её размер меньше полного пакета.

## Подпись

`latest.json.sig` — Ed25519-подпись сырых байтов `latest.json` в base64.
Подпись проверяется **до** разбора JSON: подделанный или чужой манифест не
попадает в парсер. `check::check_signed_manifest` скачивает `latest.json` и
`latest.json.sig` (HTTPS, лимит 1 МиБ/8 КиБ, отмена), проверяет подпись и
возвращает разобранный манифест. Приложение вызывает её при старте и по
таймеру; `Manifest::offer_with_dismissal` не предлагает версию, которую
пользователь отклонил вручную, но пропускает более новую.

Публичный ключ встроен в клиент:

* файл `crates/voxhold-update/update-pubkey.txt` (32 байта, base64);
* либо build-time переменная `VOXHOLD_UPDATE_PUBKEY` — для локальной приёмки.

Приватный ключ канала `beta` создан этой задачей и хранится **вне
репозитория** (точный путь передан в отчёте о выполнении и не коммитится).

В CI он задаётся секретом `VOXHOLD_UPDATE_SIGNING_KEY` (base64 PKCS#8).
`voxhold-release keygen --out <file>` печатает публичный ключ и создаёт пару.

Ротация: выпустить клиент с новым публичным ключом, затем начать подписывать
манифесты новым ключом. Схема v1 хранит один ключ, поэтому период, когда
старые клиенты ещё работают, нужно покрыть отдельным релизом; старые подписи
не переиспользуются.

## Детерминированный пакет

`VoxHold-portable.zip` собирается `voxhold-release pack`:

* файлы идут в лексикографическом порядке путей;
* фиксированное время 1980-01-01, без extra fields и directory entries;
* фиксированный уровень Deflate, unix mode 0644;
* внутри `package.json`:

```json
{"version":"0.2.0","files":[{"path":"Voxhold.exe","size":123,"sha256":"..."}]}
```

`package.json` не перечисляет сам себя. `sha256(zip)` — это `package_sha256`.
`repack_installed(dir, package_json)` собирает тот же ZIP из установленных
файлов без хранения копии пакета; хэш обязан совпасть с базовым. Установщик
кладёт тот же `package.json` в каталог установки, поэтому дельта работает и
после Setup, и после распаковки portable ZIP.
Детерминизм обеспечивает закреплённая версия `zip 9.5.1`; обновление
зависимости требует пересборки базовых пакетов канала.

## Дельта

`delta.json` внутри delta-ZIP:

```json
{
  "schema": 1,
  "from_version": "0.1.0",
  "to_version": "0.2.0",
  "base_package_sha256": "...",
  "target_package_sha256": "...",
  "deletes": ["old/file"],
  "files": [
    {"path": "Voxhold.exe", "mode": "patch", "size": 123,
     "sha256": "...", "patch": {"base_size": 100, "base_sha256": "..."}},
    {"path": "README.md", "mode": "replace", "size": 10, "sha256": "..."},
    {"path": "assets/icon.svg", "mode": "copy", "size": 20, "sha256": "..."}
  ]
}
```

* `copy` — файл берётся из базового пакета;
* `replace` — полный файл лежит в `files/<path>`;
* `patch` — блочный патч лежит в `patches/<path>` (блоки 4 КиБ,
  операции copy/literal); применяется только если он меньше замены.

После применения SHA-256 восстановленного ZIP обязан совпасть с
`target_package_sha256`, иначе `DeltaTargetMismatch`. Если базы нет, хэш базы
не совпал, дельта сломана или дельта не меньше полного пакета — клиент
скачивает полный пакет (`FullFallback`).

## Загрузка и staging

Корень: `%LOCALAPPDATA%\VoxHold\updates` (`$XDG_CACHE_HOME/voxhold/updates`
вне Windows). Каталог создаётся с наследованием ACL текущего пользователя;
отдельная установка DACL в этой версии не выполняется. Каталог версии:

```text
updates\<version>\
  package.zip        полный или восстановленный пакет
  delta.zip          скачанная дельта
  app\               распакованная следующая версия + package.json
  ready.json         version, package_sha256, install_dir, created_at, method
  apply.log          пишет helper (пути и коды, без URL)
```

Скачивание:

* только HTTPS (loopback HTTP — для тестов);
* лимит `size + 1 МиБ`, проверка `Content-Length` и фактических байтов;
* `.part` и `Range: bytes=<offset>-`; ответ 206 проверяется по
  `Content-Range`, ответ 200 начинает загрузку заново;
* отмена через `CancelToken`;
* SHA-256 проверяется после завершения; при несовпадении, отмене или
  превышении размера `.part` удаляется; при сетевом обрыве `.part`
  сохраняется для продолжения;
* URL в ошибках — без query и учётных данных.

Очистка:

* `stage_update` удаляет каталог версии при любой ошибке;
* сетевой обрыв внутри одной попытки повторяется один раз с `Range`, затем
  каталог удаляется;
* `cleanup_stale(root, keep)` на старте удаляет старые попытки и посторонние
  файлы;
* `cleanup_after_install(staging)` удаляет каталог после успешного
  применения;
* `updates\<version>` после успешной установки, отмены и ошибки подготовки
  не остаётся. Если ошибка произошла при применении, helper оставляет
  `apply.log` (и каталог версии) для диагностики; следующий старт удаляет
  их через `cleanup_stale`.

`apply_staged` удаляет только `app`; helper после успеха вызывает
`cleanup_after_install` для всего каталога версии. Коды helper:
`0` — успех, `2` — аргументы, `3` — клиент не завершился, `4` — ошибка
применения.

## Применение и откат

`voxhold-updater`:

```text
voxhold-updater --pid <pid> --target <install dir> --staged <updates\version> \
  --exe Voxhold.exe --handshake <file>
```

1. Ждёт завершения PID (до 300 с).
2. `apply_staged` переименовывает установочный каталог в
   `<name>.voxhold-backup`.
3. Копирует только пути из нового `package.json`; файлы пользователя вне
   пакета сохраняются.
4. Запускает новый `Voxhold.exe` с `--update-handshake <file>`.
5. Ждёт handshake 30 с. Успех — бэкап удаляется. Ошибка/таймаут — новый
   каталог удаляется, бэкап возвращается на место.
6. `rollback(target)` явно возвращает бэкап, если он остался.

Приложение после интеграции должно:

* завершить звонок и сохранить настройки перед запуском helper;
* при старте с `--update-handshake <file>` записать файл только после
  успешной инициализации;
* повышать права только по явному действию пользователя (helper этого не делает).

## Локальная приёмка A → B

1. `cargo run -p voxhold-release -- keygen --out <вне-репозитория>`.
2. Собрать пакеты A и B: `pack --dir <stage> --version 0.1.0 --out a.zip`
   и то же для `0.2.0`.
3. `delta --base a.zip --target b.zip --out d.zip`.
4. Поднять loopback HTTP-сервер с `a.zip`, `b.zip`, `d.zip`, `Setup.exe`.
5. `manifest --full b.zip --setup Setup.exe --version 0.2.0 --delta d.zip ...`
   с URL вида `http://127.0.0.1:<port>/...`.
6. `verify --manifest latest.json --sig latest.json.sig --full b.zip`.
7. В тесте вызвать `stage_update` с распакованным A; проверить, что скачалась
   дельта и `ready.json.method == "delta"`, а размер скачанных байт меньше
   полного пакета. Без базы, с битой дельтой и при `delta.size >= full.size`
   должен быть полный пакет.

HTTP допустим только для loopback; подпись проверяется и в этом режиме.
`VOXHOLD_UPDATE_PUBKEY` позволяет собрать тестовый клиент с другим ключом, но
не отключает проверку.

## CI

`.github/workflows/release.yml` запускается вручную (`version`, `channel`,
`publish`) или тегом `v*`:

* собирает клиент, helper и release CLI;
* подписывает `Voxhold.exe` и `voxhold-updater.exe` сертификатом из
  `WINDOWS_CERT_PFX_BASE64`/`WINDOWS_CERT_PASSWORD` **до** упаковки ZIP;
  без секретов выпускает `authenticode: false` с предупреждением;
* собирает portable ZIP, ставит Inno Setup и собирает per-user
  `VoxHold-Setup.exe` (`{localappdata}\Programs\VoxHold`, без admin);
* скачивает предыдущий `VoxHold-portable.zip` из релиза канала и делает
  дельту; при отсутствии базы или если дельта не меньше — пропускает;
* формирует `latest.json`, подписывает `latest.json.sig`, проверяет
  артефакты;
* всегда загружает artifacts, а при `publish` — ассеты в релиз `beta`
  (`gh release upload --clobber`), манифест последним.

Секреты: `VOXHOLD_UPDATE_SIGNING_KEY`, `WINDOWS_CERT_PFX_BASE64`,
`WINDOWS_CERT_PASSWORD`. Без ключа подписи публикация завершается ошибкой.

## Ограничения

* MSI и дельта для `Setup.exe` не поддерживаются: установщик скачивается
  целиком, дельта заявлена только для portable ZIP.
* Authenticode без сертификата недоступен: EXE и установщик будут
  неподписанными, SmartScreen может предупредить. Это не заменяет подпись
  манифеста. Клиент проверяет подпись манифеста и SHA-256; Authenticode
  проверяется Windows при запуске, отдельного `WinVerifyTrust` в клиенте нет.
* Схема v1 хранит один публичный ключ; одновременная ротация без выпуска
  клиента не поддерживается.
* UI-иконка, tooltip, прогресс и отмена в интерфейсе — следующий этап.
* `--update-handshake` должен быть реализован в приложении; без него helper
  откатит обновление по таймауту.
