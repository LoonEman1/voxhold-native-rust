# Журнал по этапам

История выполнения по этапам. Таблица статусов, правила ведения журнала и чек-лист PR остаются в [development-plan.md](development-plan.md), раздел 7.

### R03 · Композер, вложения и медиа — журнал PR #12

Ветка `codex/r03-composer-media`, последний коммит
[`6f044fd`](https://github.com/LoonEman1/voxhold-native-rust/commit/6f044fdce10831d4f485b51b2c9147467a90f3d1),
PR [#12](https://github.com/LoonEman1/voxhold-native-rust/pull/12) в `dev`.

- `src/draft.rs`, `src/app/composer.rs`, `src/attachments.rs`: черновики по
  `ChatKey`, идемпотентная отправка, сохранение правок и новых файлов при
  позднем подтверждении, отмена всех отправок при logout, потоковая загрузка
  и проверка неизменности файла перед повторным открытием.
- `src/file_io.rs`: атомарное сохранение через `.part`, проверка размера,
  замена цели и отказ при занятом файле.
- `src/links.rs`, `src/media.rs`, `src/app/history_view.rs`,
  `src/app/media_ops.rs`: кеши миниатюр и превью, карточки ссылок, один viewer,
  видимость запросов и таймер только по будущим срокам видимых ссылок.
- `src/history.rs`, `src/app/realtime.rs`, `src/app/history_ops.rs`: слияние
  revision и реакций, tombstone вложений по ID, ограниченный набор маркеров,
  инвалидация `/context`, refresh и older-page при вытеснении маркера,
  перезапуск одного свежего refresh после пачки событий и запрет вставки
  позднего подтверждения отправки при `access_pending`.
- Тесты: отмена отправки использует реальный upload ID, параллельные отправки
  удерживаются двумя запросами одновременно, добавлены регрессии на превью
  вне экрана, массовое удаление, отсутствующую строку `/context` и отменённую
  первую загрузку истории.

Проверено в PR: `cargo fmt --all -- --check`,
`cargo test --workspace --locked` (130 тестов приложения: 129 passed,
1 `ignored`; 5 API; 6 media), проверка размеров (124 файла). Осталось до
merge/релиза: CI на PR, живой backend и веб-пары, кадры и DPI, ручной сценарий
двух аккаунтов; подробности в [status.md](status.md).

### R04 · Действия сообщений, обсуждения, поиск и ЛС — журнал PR #13
Ветка `codex/r04-message-actions`, база `origin/dev` `88a7bad`, коммит
[`9f450e7`](https://github.com/LoonEman1/voxhold-native-rust/commit/9f450e7b380491dbbdd457e10bd1a94f8201c8b0),
PR [#13](https://github.com/LoonEman1/voxhold-native-rust/pull/13); сверены backend `9b29fdd`, frontend `ed7cb00`, Go `09c82b9`.

- `message_tools.rs`, `history_actions.rs`, `action_io.rs`, `history_view.rs`,
  `overlays_view.rs`: hover/меню, один диспетчер с `(ChatKey, access_generation,
  request_id)`, повторная проверка строки/прав, ответ/правка/удаление/реакции/
  пересылка/избранное/закреп/блокировка, копирование строк, `scroll_epoch`, `403`.
- `thread_panel.rs`, `thread_ops.rs`: отдельная лента, якорь, composer и черновик
  `ChatKey::thread`, comments/thread read, закрытие при удалении root.
- `search.rs`, `overlays_view.rs`: popup с `limit=25`/`before_id`, состояния и
  отмена старого запроса, проверка доступного канала, якорь/подсветка, pin list.
- `inbox.rs`, `inbox_ops.rs`, `voxhold-api`: недавние ЛС `limit=13` без `saved`,
  direct/group, peer/аватар, история/отправка/read/вложения/действия в workspace,
  изоляция числовых ID через `ChatKey`.
- `tests/r04.rs`, `message_tools.rs`: права по роли, stale search page, закрытый
  канал, одинаковые URL/цитаты, edit/delete в ветке, канал ↔ ЛС ↔ сервер, порядок
  копирования, ревизии реакций/закрепов, старый клик, поздний ответ.

До правок ревью прошли fmt, тесты (147: 146 passed, 1 `ignored`; 6 API; 6 media), release, размеры и smoke; на `9b29fdd` — вход, канал/ЛС, поиск, закреп, ветка, read, recents и `403`.

Первый раунд ревью — [`57184ae`](https://github.com/LoonEman1/voxhold-native-rust/commit/57184ae): пересылка по `message_id`, отложенный переход из поиска, удаление корня ветки по WebSocket, отдельная попытка отправки и закрытие панели, tombstone/слияние страницы, новостные комментарии, ACL-фильтр поиска, реакция старого сообщения ЛС, reconnect ЛС, раздельные состояния закрепов, слот прав после `403`, единый hover-диспетчер. Второй раунд — [`4d8cac1`](https://github.com/LoonEman1/voxhold-native-rust/commit/4d8cac1): поиск (включая ЛС) ждёт подтверждённого снимка каналов; POST ветки хранится в `pending_sends` по `ChatKey::thread` и виден переоткрытой панели; пустая последняя страница ветки сохраняет WS/POST-строки и отсекает tombstone; два события реакции дают один активный GET и один повтор по `(ChatKey, message_id)`. После правок: `cargo fmt --check`, `cargo test --workspace --locked` (167: 166 passed, 1 `ignored`; 6 API; 6 media), размеры. Release, GUI и живой backend после правок не перезапускались; осталось: два аккаунта, кадры/DPI, живые новостные роли и закрепы/реакции ЛС, E2EE. См. [status.md](status.md).

Третий раунд — [`4057c7a`](https://github.com/LoonEman1/voxhold-native-rust/commit/4057c7a): отзыв доступа в ЛС сбрасывает подтверждённый поисковый снимок и старые страницы, а при закрытом popup свежий `/channels` запускается следующим открытием; `403` без окна подтверждает права только через владельца слота снимка. Выбор канала переустанавливает уже подтверждённые права до загрузки закрепов и дозагружает их после нового подтверждения. Подтверждение POST ветки снимает «Отправка…» и очищает/сохраняет живой черновик по `plan.key` даже после смены канала, а вставку строки и read ограничивает `access_generation`. «К последним» заменяет старое окно, но возвращает комментарии, пришедшие по WS во время GET, и соблюдает tombstone. После правок: `cargo fmt --all -- --check`, `cargo test --workspace --locked` (173 теста приложения: 172 passed, 1 `ignored`; 6 API; 6 media), размеры (138 файлов). Release, GUI и живой backend после правок не перезапускались; осталось: два аккаунта, кадры/DPI, живые новостные роли и закрепы/реакции ЛС, E2EE. См. [status.md](status.md).

Дополнение третьего раунда — [`0b57d56`](https://github.com/LoonEman1/voxhold-native-rust/commit/0b57d56): `channels.changed` в ЛС освобождает и слот `/channels`-проверки после `403`, поэтому старый ответ не может снова разрешить поиск; запрос проверки переведён на tokio без блокировки UI. Возврат ЛС → канал при неподтверждённом снимке запускает свежий `/channels`, и только его callback синхронизирует права и загружает закрепы. Добавлены регрессии на удержание обоих GET и на DM → канал с закрепом 42. Проверки: `cargo fmt --all -- --check`, `cargo test --workspace --locked` (173 теста приложения: 172 passed, 1 `ignored`; 6 API; 6 media), размеры (138 файлов). Release, GUI и живой backend не перезапускались.

### R05 · Участники, личные и административные настройки — журнал PR #14

Ветка `codex/r05-settings`, база `origin/dev` `47f977c`, PR
[#14](https://github.com/LoonEman1/voxhold-native-rust/pull/14); сверены
backend `9b29fdd`, frontend `ed7cb00`, Go `09c82b9`.

- `members.rs`, `members_view.rs`: единый снимок участников, присутствие,
  аватары/инициалы, роли, контекстное меню и подтверждённые destructive
  действия.
- `settings.rs`, `settings_view.rs`: модальное дерево с поколением диалога,
  scope, ленивыми вкладками и `FormValue`; профиль, приватность, тема,
  уведомления, пароль, сессии и удаление аккаунта.
- `server_ops.rs`, `server_settings.rs`: owner-only общие настройки/CORS,
  роли и grants, хранилище/квоты/очистка, баны и диагностика.
- `channel_ops.rs`, `channel_settings.rs`: текстовые/новостные/голосовые
  формы, доступ, slow mode, E2EE и отдельный диалог уведомлений канала.
- `realtime.rs`: `presence.*`, `avatar.updated`, `server.member_*`,
  уведомления и `notification.settings_updated` в существующем WS.

Раунд ревью: монотонное поколение на каждом reopen всех четырёх форм и
проверка поколения/вкладки/scope в `settle` не дают старому `/access`
заполнить форму другого или заново открытого канала; кнопка уведомлений
канала вынесена из `can_manage`, форма закрывается при потере
`has_history()`; вкладка профиля хранит `idle/loading/loaded/error` и
перезапускает отменённый GET при возврате, а у сессий свой retry; PATCH
профиля блокирует только кнопку и получает свой request id; WS-настройка
уведомлений не стирает несохранённый выбор и отменяет старый GET; DELETE
аккаунта завершает сессию даже после закрытия формы, но проверяет
`session_generation`; декодирование аватара вынесено в
`background_spawn` и отбрасывается по поколению/сессии/ID выбора; PTT
передаётся в `VoicePanel` при создании, закрывает микрофон до первого
`join` и применяется к уже открытой панели.

Повторный раунд: активный PATCH профиля хранится на уровне сессии, а
активный PUT уведомлений — по паре `(server_id, channel_id)`, поэтому
`reopen` не снимает блокировку; режимы уведомлений отключены и не
принимаются обработчиком, пока PUT не ответил; расчёт превью аватара
захватывает ID файла из самого `CropDraft` и не применяет результат
старого файла при совпавшем `preview_token`. Успешный PATCH обновляет
снимок переоткрытого профиля на любой открытой вкладке настроек, сохраняя
поля, отредактированные в новой форме; GET профиля, начатый до ответа
PATCH, отбрасывается по epoch, а GET во время PATCH сохраняет локальные
поля и `dispatched` активного запроса.

Третий раунд: пока PATCH профиля активен, GET обновляет только
подтверждённый снимок и сохраняет оба поля редактора, поэтому страна из
ответа GET не подменяет отправленную и не принимается `settle` за правку
пользователя; «Отмена» заблокирована и в кнопке, и в обработчике до ответа
PATCH, поэтому не сбрасывает редактор и `dispatched`. Регрессии: разные
страны в ответах GET и PATCH с проверкой редактора и следующего PATCH,
прямой тест слияния снимка, задержанный PATCH с нажатием «Отмены».

Проверки после правок: `cargo fmt --all -- --check`,
`cargo test --workspace --locked` (230 тестов приложения: 229 passed,
1 `ignored`; 6 API; 6 media), `cargo build --release --locked
-p voxhold-native-rust`, `scripts/check-file-sizes.ps1`. Живой backend,
ручной GUI owner/admin/member, два аккаунта, кадры/DPI и фактический
balloon не проверялись. См. [status.md](status.md).

Интеграция: PR [#14](https://github.com/LoonEman1/voxhold-native-rust/pull/14)
слит в `dev` коммитом
[`bf421cc`](https://github.com/LoonEman1/voxhold-native-rust/commit/bf421ccf386fb392d3f166a0d11b96d9e7ea2731)
поверх `47f977c`; Windows CI на `33d9a7a` завершился успешно
([run 43930378048](https://github.com/LoonEman1/voxhold-native-rust/actions/runs/43930378048)).
После merge ручной прогон R05 не выполнялся: на локальной машине не было
запущенного backend и тестовых owner/admin/member, поэтому вход, вкладки,
поздние ответы, `403`, отзыв сессии и восстановление настроек после
перезапуска остаются непроверенными вживую. Следующее задание по плану —
`R06`.

### R06 · Записи сервера (Go W01) — журнал PR #16

Ветка `codex/r06-recordings`, база `origin/dev` `8919962` (merge PR #15),
PR [#16](https://github.com/LoonEman1/voxhold-native-rust/pull/16); сверены
backend `9b29fdd`, frontend `ed7cb00`, Go `09c82b9`.

- `crates/voxhold-api/src/recordings.rs`: DTO `Recording`/`RecordingList`,
  `list/get/delete`, приватный JPEG-preview (128 КиБ, JPEG, ≤1280×720 и
  4 Мип), тикет только по относительному
  `^/api/v1/recording-downloads/[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$`, поток без
  Bearer и редиректов, лимит 2 ГиБ, проверка `Content-Length`/итогового
  размера, timeout `size/(512 КиБ/с)+60 с`, минимум 120 с.
- `src/recordings.rs`, `src/recordings_dialog.rs`: снимок сервера с
  `confirmed_epoch`/`access_epoch`, `request_id`/`event_seq`, `touched`,
  tombstones, bounded retry (3), фильтр чужих/истёкших/удалённых строк,
  сортировка live → `created_at` ↓ → `id` ↓; геометрия
  `min(570,w-16)`, список ≤`0.46*h`, карточка 132×74 / stacked `<360`,
  retry 88×32, закрытие 98×38, confirm и hitbox∩list.
- `src/app/recordings_view.rs`, `recordings_ops.rs`,
  `recordings_download.rs`: одна модалка из шапки (справа от «Поиска») и
  панели звонка; Esc/клик вне/«Закрыть» возвращают фокус; Tab/Shift+Tab по
  нижним карточкам, Enter на обрезанном действии только прокручивает,
  колесо сбрасывает отложенный прыжок; `recording.updated`, `ready`,
  `channels.changed` и `server.member_*` обновляют `access_epoch`; список
  продолжает работать в ЛС; красный маркер канала и «Идёт запись · имя»
  читают один снимок.
- Сохранение: Windows picker → билет → потоковое `.part` → проверка
  размера → `MoveFileEx`; отмена не запрашивает билет, поздний picker
  проверяет поколение/epoch. Удаление с сервера доступно только владельцу
  записи и не воскрешается поздним GET/WS.
- Регрессии: `recordings_retry_keeps_newer_event`,
  `recordings_hide_metadata_while_access_is_unconfirmed`,
  `recording_preview_waits_for_access_confirmation`,
  `recording_preview_stops_when_dialog_closes`,
  `recording_enter_on_clipped_action_waits_for_scroll`,
  `recording_wheel_after_click_scrolls_to_list_end`,
  `recording_release_in_clipped_part_cancels_press`, пустая версия preview,
  delete → WS → GET 404, 24 МиБ за >8 с и кадры 1440×900 / 900×720 /
  390×760 в тестовом окне плюс чистая геометрия.

Проверки после правок: `cargo fmt --all -- --check`,
`cargo test --workspace --locked` (254 теста приложения: 253 passed,
1 `ignored`; 11 API; 6 media), `cargo build --release --locked
-p voxhold-native-rust`, `scripts/check-file-sizes.ps1` (158 файлов),
smoke release (процесс активен через 4 с). Живой backend, реальная
загрузка/удаление записи, второй клиент для `recording.updated`, ручной
GUI, Windows save dialog, кадры/DPI и визуальные пары Go/Rust не
проверялись. См. [status.md](status.md).

### R07a · Голос и просмотр текущего Go — журнал PR #18

Ветка `codex/r07-voice-viewer`, база `origin/dev` `e6c85bf` (merge PR #16),
PR [#18](https://github.com/LoonEman1/voxhold-native-rust/pull/18); сверены
backend `9b29fdd6d5dc818b7408e4892b8e68b27d123831`,
frontend `ed7cb00ae12398753a8ef48a5a33b864e20cb04c` и Go
`09c82b983d5d842af0627289655b11c37ae56e1d`.

- `crates/voxhold-media/src/catalog.rs`, `viewer.rs`: детерминированная модель
  просмотра без WebRTC и таймеров. Публикация адресуется
  `(server_id, channel_id, publisher_connection_id)` и номером попытки;
  смена B→C ждёт `stream.left` A (5 с), затем запускает ровно C; offer/ICE
  старой попытки и `stream.stopped(A)` не трогают активный B; `stream.watching`
  принимается только по своему `request_id`; ошибка `native-stream-N`
  сравнивается с `request_start`; reconnect сохраняет выбранную публикацию до
  нового `voice.joined`.
- `runtime.rs`: ограниченные last-value слоты `Requests` (controls, watch,
  stream mute, viewport, local mute), коалесцирующая очередь `Updates`
  (терминальные `Error`/`Left` не сливаются, снимки заменяются), bounded
  `device notice`; отдельный поток кадра забирает и переиспользует буфер,
  кадры не копятся. Leave приоритетнее всего, `leave_stream` сбрасывает
  очередь выбора. Регистрация микрофона закрыта до подтверждённого
  `voice.joined`; `Media::Start` больше не открывает захват.
- `controller.rs`, `voice.rs`: join attempt, server mute, roster по
  `connection_id`, `connections_with_audio` для локального mute; смена
  устройства сначала поднимает новый тракт, при отказе восстанавливает
  прежний GUID/имя и отдаёт `Event::DeviceError`; default-resolve идёт через
  Windows endpoint, а не индекс 0; раз в секунду проверяется hot-unplug и
  выполняется fallback. Local принадлежит `VoiceHandle`, join/server mute —
  worker; `State` не перезаписывает local. Drop старого `Media` закрывает
  только транспорт, а защёлку ставит `VoiceHandle::leave`; выбранный выход
  отделён от применённого; отказ, unplug и явный выбор идут через Session.
  Громкость 0–200 % ждёт PCM `R07b`; микшер Windows не меняется, 0 %/deafen закрывают треки.
- `video.rs`: libyuv пишет BGRA напрямую (`I420ToARGB` на little-endian), UI
  больше не меняет каналы на каждый пиксель; синтетический I420 с
  известными красным/синим проверяет порядок каналов, отсутствие
  вертикального переворота и растяжение при scale.
- `src/voice_room.rs`, `src/voice_panel.rs`, `voice_panel/{view,player}.rs`:
  заголовки/статусы комнаты, карточки участников по сетке Go, индикатор
  речи, roster/badge канала, dock 98 px, порядок действий микрофон → звук →
  записи → выход, локальный mute только при реальном треке, Tab/Shift+Tab по
  видимым действиям. Плеер: 360 px, drag за шапку, corner resize, поля 8 px,
  ограничения 260–720, `width*9/16+76`, внутренные области Go, fullscreen,
  Esc/F/двойной клик, mute только при `has_audio`, закрытие оставляет голос.
  Просмотр комнаты и звонок независимы: переход в другой канал/ЛС не
  завершает звонок, появляется dock, `voice_room` и `call_channel`
  очищаются при leave/logout/revoke.
- `src/app/{voice_ops,voice_tests,realtime,workspace,shell_view,navigation_view}.rs`:
  подтверждённый доступ к комнате, roster всех видимых голосовых комнат и
  публикаций из account WS, PTT по сохранённой клавише с release на key-up,
  потере фокуса, модалке и уходе из комнаты; hotkey mute/deafen; устройства,
  качество и громкость из вкладки «Голос и звук» доходят до живого тракта.
  Подтверждение каналов после `select_server` и ACL-reload идёт через один
  переход: снимает `voice_unconfirmed`, чистит закрытые комнаты и
  синхронизирует панель; активная комната скрывает roster/streams, пока ACL
  не подтверждён. Потеря WS до `voice.joined` публикует восстановление для
  текущего выбора, а `Ended` закрывает плеер через путь с `Window` и
  возвращает обычный режим, если fullscreen включал сам плеер.
- Регрессии: A→B→C, `stream.stopped(A)`, повторная публикация, закрытый
  звонок/`request_id`/stale ICE, reconnect/revoke, leave-timeout,
  snapshot/stop/re-entry, `Requests`/`Updates`, BGRA, Tab/Shift+Tab, Esc/F,
  PTT, dock. Добавлены PTT/State, drop Media, Watch→Close, tagged events, P2P,
  ICE, reconnect, rollback/unplug выхода, ACL и fullscreen.
- Wire: `stream.watch` несёт `watch_id`; клиент строго проверяет метку, если
  backend её прислал. Закреплённый backend не размечает push offer/ICE/close:
  первый немаркированный close после смены попытки игнорируется; offer с неизвестным ufrag и строгая гарантия требуют watch id и буферизации до ACK.
Проверки: `cargo fmt --all -- --check`, `cargo test --workspace --locked`
(296 тестов приложения: 296 passed, 1 `ignored`; 11 API; 54 media),
`cargo build --release --locked -p voxhold-native-rust`,
`scripts/check-file-sizes.ps1` (172 файлов), `git diff --check`, smoke
release (процесс активен через 5 с).

Не проверено: живой backend/два клиента, реальный SFU/P2P с Chromium,
фактические кодеки/разрешение/задержка/CPU, пары кадров Go/Rust, DPI
100/125/150, ручной GUI, реальная смена/отключение устройств и сон,
слышимый тест микрофона/звука. Тест звука и input gain относятся к
аудиотракту `R07b`; локальный peak meter показывает реальный `MediaSource`,
но не является измерением DSP.

### W02b · Настройка параметров голоса — журнал PR #21

Ветка `codex/w-extensions`, база `origin/dev` `e1c0ff2` (merge PR #22, `R07b`),
PR [#21](https://github.com/LoonEman1/voxhold-native-rust/pull/21); сверены
backend `9b29fdd6d5dc818b7408e4892b8e68b27d123831`,
frontend `ed7cb00ae12398753a8ef48a5a33b864e20cb04c` и Go
`09c82b983d5d842af0627289655b11c37ae56e1d`.

- `src/voice_settings.rs`: снимок голосовых настроек — оба режима
  (`input_mode`/`noise_mode`), устройства по endpoint id, gate, AGC и
  усиление, пресеты 24/32/48/64/96/128, тон сигнала; нормализация диапазонов,
  атомарная запись, миграция прежних WinMM-имён и flush отложенной записи при
  закрытии, чтобы последняя правка не терялась.
- `src/app/settings_voice_view.rs`, `settings_slider.rs`, `voice_form_text.rs`:
  форма в табе «Голос и звук» — сегменты режимов, блок устройств, риска порога,
  слайдеры 0–200 и 0–100 с перетаскиванием, кликом, шагом 1, Home/End и
  pointer capture за границей карточки, пресеты битрейта, тест микрофона и
  сигналов, подписи ограничений из active capture.
- `src/app/voice_settings_ops.rs`, `voice_panel/controls.rs`: один атомарно
  применяемый audio snapshot, выбор устройства по id с отказом от устаревшего,
  запись и перечисление устройств вне UI-потока с коалесингом; индикатор в
  звонке учитывает применённый input gain и mute.
- `crates/voxhold-media/src/preview.rs`, `capabilities.rs`: тон и RMS по
  формулам Go, capability-таблица для software и platform ADM; AEC не
  заявляется без render-reference.
- Тесты: форма и оба режима через клики, 125/150 % логического viewport и
  зависимость карточки от масштаба вью, hot-unplug через `voice_meter_tick`,
  ровно один `Command::State` на изменение, коалесинг записи, слайдеры,
  round trip `mute/gate/AGC/gain` на PCM через Opus.

Ревью: два раунда в PR #21, итог PASS на `f7d142e`. Первый раунд дал 5 P1
(двойная кодировка пользовательских строк, ложные заявления о gain и AEC,
кнопки вместо слайдеров, отсутствие обязательных тестов) и 4 P2; второй — 6 P2
(потеря отложенной записи при выходе, индикатор без input gain, неполный PCM
round trip, pointer capture, слабый тест масштаба, устаревший журнал). Все
закрыты.

Проверено: `cargo fmt --all -- --check`, `cargo test --workspace --locked`
(453 passed, 0 failed, 10 `ignored`: 11 API, 107 media, 335 приложения),
`cargo build --release --locked -p voxhold-native-rust`,
`scripts/check-file-sizes.ps1 -All` (194 файла), `git diff --check`, детектор
кодировки по всем отслеживаемым `*.rs`/`*.md`.

Не проверено: живое WinMM-устройство, слышимый тон и тест микрофона, реальный
hot-unplug, физический DPI 125/150 (тесты используют параметр масштаба вью),
живой backend и браузер, AEC render-reference, smoke release на живом сервере.
Порог AGC измерен на закреплённом WebRTC и может потребовать пересмотра при
смене версии.

Журналы этапов вынесены из плана в этот файл, потому что
`docs/development-plan.md` достиг лимита 1200 строк; таблица статусов, правила
журнала и чек-лист PR остались в разделе 7 плана.

### W02c · Микшер, клавиши и восстановление — черновик к ревью

Ветка `codex/w-extensions`, база `609bb8651053e282d0ceeacd8488fd10292b8525`;
сверены Go `09c82b983d5d842af0627289655b11c37ae56e1d`, frontend
`ed7cb00ae12398753a8ef48a5a33b864e20cb04c`, backend
`9b29fdd6d5dc818b7408e4892b8e68b27d123831`. W02c закоммичен серией
`b097c0f` → `f2db984` → `e7f2820` → `d3fec1a` → `180df01`; последний
проверенный коммит этапа — `180df01`, дерево чистое и запушено.

- `crates/voxhold-media/src/mixer.rs` — программный микс шин `voice/system/
  preview`, громкость источника 0–200 % и **отдельный** признак локального
  mute/deafen (снятие mute возвращает сохранённый процент, а не запомненный
  ноль), блочный peak-лимитер с потолком 0,98, RMS/peak и уровень после микса
  по шкале WinMM-превью (×4,5), p50/p95 времени блока (nearest rank, окно
  512), счётчики starvation и drop, ограниченные очереди 5 блоков и 64
  источника. Тесты: две известные синусоиды (400/800 Гц) суммируются и
  измеряются после микса, 200 % дают удвоение, две полные шкалы упираются в
  лимитер, mute сохраняет gain, 0 % потребляет поток, p95 по известному ряду.
- `crates/voxhold-media/src/voice/playout.rs`, `voice/winmm_out.rs` — реальный
  тракт комнаты: по одному `NativeAudioStream` (48 кГц mono, очередь 8 кадров)
  на удалённую дорожку, свой поток микширования и WinMM `waveOut` на четыре
  10-мс буфера. ADM playout выключается **только если** sink открылся; иначе
  остаётся ADM и UI сообщает, что численное усиление не применяется.
  Смена устройства открывает новый sink до замены старого, откат сохраняет
  прежний путь. `Command::Volume{gain,muted}` несёт процент и отдельный mute;
  в ADM-fallback mute по-прежнему выражается `track.set_enabled(false)`.
- `crates/voxhold-media/src/controller.rs`, `src/voice_settings.rs`,
  `src/voice_panel/controls.rs`, `src/app/settings_voice_view.rs` — громкость
  участника хранится по стабильному `user_id` (`UserVolumes` в voice JSON,
  как `voxhold.voice.userVolumes.v1` во фронтенде) и разрешается в текущий
  `connection_id` через ростер и track owner; при повторном использовании
  connection id другим пользователем процент не переезжает, а неизвестный
  владелец даёт 100 %. Слайдеры участников появляются в табе только в звонке.
  Локальный mute остаётся отдельным множеством. `server_mute` из ростера
  закрывает микрофон даже при удержанном PTT и показывается в комнате.
- `src/app/hotkeys.rs`, `src/app/shell_view.rs`,
  `src/app/settings_voice_view.rs` — запись сочетания следующим нажатием
  (Escape отменяет, одиночный модификатор не считается), конфликт сравнивается
  по канонической форме (`Control+M` = `Ctrl+Shift+m` = `Shift+Ctrl+M`),
  «disable while typing» закрывает все три действия при фокусе в composer
  (как `isEditableKeyboardTarget` во фронтенде), blur/смена активации окна и
  suspend снимают PTT. Глобального хука в сборке нет: заявлен только
  window-режим, строка формы это прямо говорит.
- `src/app/power.rs`, `crates/voxhold-media/src/runtime*` — подкласс главного
  окна `SetWindowSubclass` ловит `WM_POWERBROADCAST` и только кладёт переход в
  очередь; foreground-таймер вызывает `Voxhold::power_event`, панель снимает
  PTT и отправляет `Suspend`/`Resume` через существующий слот запросов.
  Media на resume переоткрывает sink и заново привязывает выжившие receiver-
  дорожки; на suspend отпускает устройство. Для software-микрофона добавлена
  проверка присутствия WinMM-устройства (unplug выбранного микрофона ведёт в
  обычную лестницу восстановления), выход по-прежнему проверяется
  `check_devices`, включая WinMM-имена для software-sink.
- `VoiceUpdate::Playout` публикует измеренные уровень, peak, p95, underrun и
  drop; комната и таб показывают эти числа только при открытом software-
  микшере. ICE/TURN не перезаявляются: конфигурация по-прежнему приходит из
  `webrtc_config()`, живой relay не проверялся.
- Тесты: `mixer::tests::*` (10), `controller::tests::
  a_user_volume_never_moves_to_another_participant_after_reconnect`,
  `a_150_percent_user_volume_reaches_the_participant_track`,
  `src/app/hotkey_tests.rs` (запись/конфликт/Escape, typing guard, server mute,
  suspend/resume через реальный обработчик), `voice_settings_tests::
  a_per_user_volume_reaches_the_snapshot_and_is_removed_at_one_hundred`,
  `the_playout_report_is_kept_for_the_measured_mixer_line`.
- Проверено: `cargo fmt --all -- --check`, `cargo test --workspace --locked`
  (11 API, 134 media с 9 `ignored` аудио-пробами, 346 приложения с 1
  `ignored`, итого 491 passed), `cargo build --release --locked`,
  `scripts/check-file-sizes.ps1 -All` (208 файлов), `git diff --check`,
  строгий UTF-8/BOM/U+FFFD-детектор по 141 отслеживаемому `*.rs`/`*.md`,
  mojibake-скан, smoke release (процесс жив через 5 с).

Не проверено: живое аудио и слышимый микс двух тонов на реальном устройстве,
реальный сон/пробуждение Windows, hot-unplug выбранного микрофона и выхода,
живой backend/браузер, TURN/ICE на втором ПК, физический DPI 125/150, численная
громкость трансляции (плеер сохраняет отдельный ADM-движок и mute; системная
шина не входит в микс комнаты). p95 и счётчики получены на синтетических
блоках и на реальном sink в smoke-прогоне не снимались.

#### W02c · третий и четвёртый циклы правок после FAIL (до `180df01`)

- `voice.rs`, `voice/playout/session.rs`: первый sink обычного звонка больше не
  обходит машину. `Session::new` сохраняет открытый sink и передаёт его в
  `adopt_initial_playout`, который переводит `SinkRecovery` в `Unstable`
  (`opened()`); через 2 с и 50 блоков `tick_playout` даёт `Stable` и очищает
  бюджет. Хелпер общий для конструктора и headless-теста, поэтому удаление
  `opened()` роняет тест. Тест `the_first_sink_proves_stability_through_tick_playout`
  идёт через реальную точку входа (`adopt_initial_playout` → `tick_playout`,
  headless Session), затем один отказ даёт `Opening`/`Retry(0,5 с)`.
  Ограничение зафиксировано честно: сам `Session::new` открывает реальные ADM и
  WinMM-микрофон, автоматического теста на него нет — для прямой проверки
  конструктора нужен инъектируемый аудиобэкенд.
- Смерть sink и ADM-fallback отправляют финальный
  `Event::Playout{running:false, phase}`; строка микшера в настройках строится
  `mixer_status_line` и скрывается, когда `software_mixer()` ложен. Тесты:
  `the_fifth_failure_emits_one_message_and_a_final_report` (ровно одно
  `DeviceError` на переход, финальный отчёт), `the_mixer_line_disappears_when_the_software_mixer_stops`.
- ADM-здоровье: пустая комната без inbound-дорожек и отсутствие стата — это
  «неизвестно», не отказ; перезапуск и сообщение только при реально
  остановившемся `MediaPlayout.total_samples_duration`
  (`adm_health_requires_real_pcm_only_while_audio_is_expected`).
- `switch_playout` сбрасывает и окно стабильности (`Stability{since,blocks}`), и
  фазу `SinkRecovery`: новый девайс публикуется как `Unstable`, не наследует
  доказательство старого и не показывается как «стабильный микшер»
  (`switching_the_device_reports_an_unstable_sink`,
  `switching_the_sink_restarts_the_stability_window`).
- `Tap`: медленный подписчик не удаляется; пропуски считаются в
  `TapReceiver::dropped()`, `try_recv` возвращает `Block`/`Empty`/`Closed`.
  `Drop for Playout` останавливает поток флагом; завершённые track-таски
  вычищаются (`prune_finished`). Предупреждения `TapRead`/`TapReceiver` и
  неиспользуемого `opened_at` устранены.
- `voice.rs`: ADM-ветка `resolve/activate` не выполняется, если software-sink
  уже открыт, поэтому отказ ADM не сбрасывает выбранный output.
- Тесты: `voice::playout::tests::*` (13), включая
  `the_first_sink_proves_stability_through_tick_playout`,
  `the_fifth_failure_emits_one_message_and_a_final_report`,
  `switching_the_sink_restarts_the_stability_window`,
  `adm_health_requires_real_pcm_only_while_audio_is_expected`,
  `a_sink_that_opens_and_dies_still_spends_the_shared_budget`.
- Мутации: `voice_room.rs:339` (`!server_muted`), `voice_panel.rs:697`
  (`self.server_muted → false`), `SinkRecovery::sink_died` без учёта, удаление
  `opened()` из `adopt_initial_playout` и снятие сброса фазы при смене
  устройства роняют свои тесты. Файлы восстановлены из копий, мутационных строк
  нет.
- Проверено: fmt; `cargo test --workspace --locked` — 11 API, 134 media
  (9 `ignored`), 346 приложения (1 `ignored`), итого 491 passed; release build;
  размеры (208 отслеживаемых файлов, все новые в индексе); строгий
  UTF-8/BOM/U+FFFD по 141 отслеживаемому `*.rs`/`*.md`, mojibake-скан; smoke
  release — процесс жив через 5 с. Новых release-предупреждений нет; два прежних
  (`recordings_download` unused `id`, `set_allowed_channels`) остались.

Шестой, финальный гейт (`12c4b2d`) — PASS: дельта `099c27c..12c4b2d` содержит
только доки и комментарий `adopt_initial_playout`, исполняемый код не изменён;
числа `141` (`130 .rs` + `11 .md`), `208` и `491` подтверждены фактическим
прогоном; `149` не встречается в репозитории. Готовность к merge подтверждена
отдельно: P0/P1 нет, инварианты R07b и W02b целы.

Профиль микшера: синтетические 400/800 Гц, 1000 блоков; debug p50 15–21 мкс,
p95 20–38 мкс за пять прогонов (у ревьюера 16/21), release p50/p95 1/1 мкс;
лимитер 0,49 → 0,99 за 195 блоков / 1950 мс. Это CPU-время теста, не замер
устройства; `latency_ms` — блоки в буфере sink.

Не проверено: живое аудио и слышимый микс, реальный сон/пробуждение,
повторное включение ADM после fallback на железе, hot-unplug, живой
backend/браузер, TURN/ICE на втором ПК, DPI 125/150, запись W07 через `Tap`.

### W03 — публикация экрана через SFU (SDR) — журнал PR #23

Ветка `codex/w03-screen-publish`, база `fbbab95` (`dev` `ad0a9e8` + docs), PR [#23](https://github.com/LoonEman1/voxhold-native-rust/pull/23); эталоны backend `9b29fdd6d5dc818b7408e4892b8e68b27d123831`, frontend `ed7cb00ae12398753a8ef48a5a33b864e20cb04c`.

- `crates/voxhold-media/src/publisher.rs`, `publisher/engine.rs`, `publisher/engine/{loopback,lifecycle,capture}_tests.rs`: состояния попытки, двухфазный старт (`stream.start` только после готового движка), ответ на серверный offer, ICE, stop/left/closed, очередь 64 и нефатальное переполнение ICE, `Drop` закрывает канал до `join`. Loopback-тест фиксирует mid/`a=sendonly` и приход кадров.
- `crates/voxhold-media/src/screen_capture.rs`: `libwebrtc::desktop_capturer::DesktopCapturer` (2 монитора/14 окон на проверочной машине), фильтр окон своего процесса, BGRA→I420 box-downscale, bounded latest, preview ≤640 px/4 МиБ, `Temporary` ждёт watchdog, `Permanent` терминален.
- `crates/voxhold-media/src/runtime/screen.rs`, `runtime.rs`: проводка команд пикера, generation листингов, `ScreenShare`-апдейты, освобождение capture при leave, конфликт с pending viewer, test-support seam.
- UI: `src/voice_panel/screen_share.rs`, `screen_share/dialog.rs`, `view.rs`, `app/{mod,shell_view,voice_ops}.rs` — кнопка «Экран», модалка только с SDR-настройками и подтверждённым VP8, карточка «Ваш экран», фокус/клавиатура, cancel/stop, причина disabled в активной фазе.
- `live_tests.rs` + `live_probe.rs`: `#[ignore]` live-тест на Docker-стенде, публикатор и viewer через реальный Pion, все ожидания с дедлайнами. Прогон: join 50 мс, Live 252 мс, `answer_mid=0`, 10 кадров/152 пакета, viewer 18 кадров за 2.01 с, stop 151 мс.
- Найден и исправлен дефект R07a: снапшот до `ready` выбрасывался `set_joined(false)`; регрессия `viewer::tests::a_snapshot_that_arrives_before_ready_starts_after_the_join`.
- Ревью: раунд 1 — не PASS (P1-1: наследование `stop_sent`/`connected`; P1-2: `stream.start` после провала старта; P1-3: очередь 16 и зависание `Drop`), исправление `aedd22b`; раунд 2 — PASS; раунд 3 на `7fd24e1` — P0/P1 нет, единственный P2 (warning `had_engine`), исправление `f33175c`; раунд 4 — PASS.
- Проверки: fmt; тесты API 11 / media 179 (11 ignored) / app 353 (1 ignored), 0 failed; release; 223 файла в лимитах; кодировка чистая; мутации track/stop/viewer/reset падают на нужных тестах.
- Осталось: браузерный viewer без микрофона, TURN/внешний backend, hot-unplug, DPI 125/150, sleep/resume, задержка и FPS/CPU/GPU-замеры; HDR/P2P/system audio/запись — W04a/W04b/W11/W12.

### W04a — системный звук и качество — журнал PR #24

Ветка `codex/w04a-system-audio`, база `dev` `79f1e51` (merge W03), PR [#24](https://github.com/LoonEman1/voxhold-native-rust/pull/24); эталоны backend `9b29fdd6d5dc818b7408e4892b8e68b27d123831`, frontend `ed7cb00ae12398753a8ef48a5a33b864e20cb04c`.

- `system_audio.rs` + `system_audio/{format,wasapi}.rs`: WASAPI loopback default render endpoint в shared mode, 48 кГц stereo i16, конверсия float/PCM16/24/32 и 44,1→48 кГц, downmix 5.1/7.1 (center/surround −3 дБ, LFE не включается), перенос хвоста пакета, тишина, bounded очередь, reopen устройства, COM на потоке захвата, все таймауты ограничены. Ручной тест: default render `48000/2/32f`, тон `blocks=118 peak=8000`.
- `publisher/engine.rs` + `engine/audio.rs`: `NativeAudioSource` 48 кГц stereo, EC/NS/AGC off, audio m-line после video, `has_audio` фактический, лимит 64–320; mic отдельно; preview без звука.
- `publisher/quality.rs` + UI: пресеты FPS/битрейт, аудио 64–320, restart = один `stream.stop` + новый `stream.start` после ack; поздние события старой попытки игнорируются, revoke завершает. Пока асинхронная проба устройства не ответила, Start/Restart заблокированы, тумблер «Включён» не показывается.
- P2-фиксы: `9333eda` (downmix, remainder, фоновая проба, тест тишины, mic/system separation с реальным gate, кнопка «Качество» только в Live) и `7440ab6` (Start при незавершённой пробе); A/V offset зафиксирован как ограничение.
- Live: join 50 мс, Live 302–803 мс, 97 audio packets, 1,2–126,7 кбит/с при 128, restart 302–855 мс, viewer 108–121 audio packet и 3–17 кадров, stop 50–150 мс.
- Проверки: 570 тестов (11/199/360), release, 230 файлов, кодировка; мутации downmix/remainder/probe/silence/stream-id/quality/pending-start падают на нужных тестах.
- Ревью: раунд 1 — PASS + 7 P2; раунд 2 — PASS + новый P2; раунд 3 — PASS после `7440ab6`.
- Осталось: слышимый звук, web-viewer, физический 5.1/7.1, отключение устройства, 1080p/60, 320 кбит/с, DPI, смена качества при активном зрителе; A/V offset и source-change — perf-этап.

### W09e–W09g — сессия, запомненный инстанс и вход — PR #25

Ветка `codex/w09e-session`, merge `a6292cd`; база `276de4b`. Проверки: fmt; `cargo test --workspace --locked` (API 13 + WS, media 199 + 12 ignored, app 464 + 1 ignored, 0 failed); release; лимиты файлов. Три независимых ревью, финальный PASS на `92ba059`.

- `W09e` (`f343eec`): `Session` — единственный владелец user/token/expiry/generation; login `{user,session}`, refresh `{token,expires_at}`, logout Bearer; refresh за 5 минут, single-flight, bounded backoff; 401 называет фактически отправленный token; 401 текущего токена во время своей ротации откладывается; после rotate перезапускаются account WS, подписка канала и голос; logout локально завершает WS/медиа/голос/pending sends и делает один best-effort POST.
- `W09f` (`167211c`): `%APPDATA%\Voxhold\instance.json`, только канонический origin, атомарная запись; галочка «Запомнить адрес»; CLI `-server` приоритетнее; битая запись и ошибка save читаемы; карточка поднимается в низком окне.
- `W09g` (`92ba059`): DPAPI CurrentUser `sessions.bin`, magic+entropy, зануление расшифрованного буфера; запись привязана к точному origin и account; автовход после подтверждения адреса, retry при сети, очистка при 401/logout; поколение store проверяется под in-process и named lock; refresh обновляет запись; реальный WS-тест двух origin.
- Не проверено: живой backend/Docker, ручной GUI, два Windows-пользователя/процесса, физический DPI, реальная ротация на сервере.

### W12b — канал обновлений, Setup и portable ZIP — PR #26

Ветка `codex/w12b-updates`, `52b30ea`; база `dev` `a6292cd`. Проверки: fmt; `cargo test --workspace --locked` (782 passed, 0 failed, 13 ignored); release; лимиты файлов 274; локальный pack/delta/manifest/verify (дельта ≈1,6 % полного), подмена подписи/пакета отклоняется. Ревью core и UI — PASS после раундов P1/P2.

- `crates/voxhold-update`: строгий schema-1 manifest, Ed25519 до парсинга, HTTPS/loopback, semver/downgrade/channel/min_windows, детерминированный ZIP, `package.json`, дельта copy/replace/блочный патч, `VerifiedManifest`, staging `%LOCALAPPDATA%\VoxHold\updates`, `Range`/206, SHA/размеры, full fallback, cleanup, apply/backup/rollback/helper relocation/handshake.
- `crates/voxhold-updater`: helper вне заменяемого каталога, абсолютные пути, `--detached` только внутренний, GUI subsystem, backup cleanup, best-effort stale cleanup.
- `crates/voxhold-release`: keygen/pack/unpack/delta/manifest (`--base-url`)/verify.
- Приложение: `build.rs` версия/ключ, `src/update.rs`, иконка/панель в шапке, check при старте/6 ч/вручную, download/cancel/progress, skip version, ready/install, handshake, startup cleanup, `VOXHOLD_UPDATE_BASE` для локальной приёмки.
- CI `release.yml`: одна Windows x64 сборка → portable ZIP + per-user Setup.exe (Inno), Authenticode при секретах, дельта от предыдущего `beta`, manifest+sign последним.
- Ключ: публичный в `crates/voxhold-update/update-pubkey.txt`, приватный в GitHub secret `VOXHOLD_UPDATE_SIGNING_KEY` и вне репозитория; Authenticode-сертификата нет (`authenticode:false`).
- Не проверено: реальный A→B из Setup и ZIP на чистой Windows, компиляция Inno/установка/удаление, Authenticode/SmartScreen, реальный GitHub-канал, живой GUI/DPI и настоящий helper-процесс.
- Интеграция: PR #26 влит в `dev` коммитом `78a7cb3`; ревью и тесты пройдены на `2ceb420`. Реальный A→B, Inno и Authenticode остаются обязательными сценариями `W13`.

### W05a — текстовый чат голосовой комнаты — PR #27

Ветка `codex/w05a-room-chat` от `78a7cb3`, коммит `6345627`; эталоны backend `9b29fdd`, frontend `ed7cb00`. Проверки: fmt; `cargo test --workspace --locked` — 791 passed, 0 failed, 13 ignored (API 15, media 199/12 ignored, app 497/1 ignored, release 3, update 72, updater 5); release; лимиты файлов 277.

- `src/app/room_chat.rs`: открытие/закрытие панели для выбранной комнаты, `ChatKey::channel(server, room)`, `chat_scope_epoch`, док/стек-панель, очистка при отзыве доступа; `Channel::chat_readable/chat_writable` в `crates/voxhold-api/src/channel.rs` с сохранением E2EE-политики.
- `src/app/shell_view.rs` + `composer.rs`: переключатель «Чат», общий composer и history в доке; `history_view` учитывает 48 px заголовок панели.
- `src/app/workspace.rs`, `history_ops.rs`, `history_actions.rs`, `session.rs`, `message_tools.rs`: подписка и read только для видимой разрешённой ленты, epoch для отложенного read, prune после подтверждённого списка каналов, права действий для комнаты.
- `src/layout.rs`: `voice_chat_layout` — 300/34%/440 справа на широком окне, непалящее наложение на узком; `VoicePanel` получает фактический размер.
- Ревью диапазона `78a7cb3..6345627` — PASS, P0/P1 нет. P2: единственный переключатель без подписи на широком окне; недоступные «Закрепы» комнаты; снапшот без снятия подписки при отзыве Read; неполное покрытие failure-path; cfg-стабы без `voxhold-media`.
- Не проверено: живой backend/POST/WS, ручной GUI и DPI, сравнение кадров web ↔ native; P2 вынесены на последующие правки/`W13`.

### W08a — полноценные «Сообщения» и новая беседа — PR #27

Ветка `codex/w08a-messages` от `13fd803`, итог `d0a6f50`; база `dev` обновлена до `922d41b` (общий Cargo-кеш и CI из PR #28). Эталоны backend `9b29fdd`, frontend `ed7cb00`. Проверки: fmt; `cargo test --workspace --locked` — 829 passed, 0 failed, 13 ignored (API 15, media 199/12 ignored, app 535/1 ignored, release 3, update 72, updater 5); release; лимиты файлов 284, `messages_ops.rs` 1143 с документированным исключением 1200.

- `crates/voxhold-api/src/conversations.rs`: типизированные paged list, conversation GET, create (direct dedup), message search с numeric cursor, notification GET/PUT, leave; проверки id/курсоров/лимитов.
- `src/messages.rs`, `src/app/messages_ops.rs`, `messages_view.rs`, `messages_creator.rs`: личный режим, bounded первая/старая страницы, фильтр по имени и глобальный поиск, `+` с выбором участников, mute/leave, `conversation.changed`, точечная проверка отсутствующей строки через `GET`.
- `src/app/shell_view.rs`, `navigation_view.rs`: rail «Сообщения», sidebar/center, «К каналам» с восстановлением разрешённого контекста, voice dock и account footer в личном режиме, ошибки/повтор первой страницы.
- `src/app/realtime.rs`, `session.rs`, `workspace.rs`, `history_ops.rs`, `composer.rs`, `inbox_ops.rs`: события личного индекса независимы от channel ACL, сброс состояния при logout, сохранение DM-черновика при смене/удалении сервера, merge успешной отправки при висящей ACL-перезагрузке, сохранение черновика и вложений при «+», 403 block/privacy не закрывает диалог (проверка `GET`), 404/leave очищают ChatKey.
- Ревью: три раунда. Первый — 3 P1 (частичная страница, logout, voice dock) + P2; второй — 2 P1 (классификация 403, потеря черновика при «+») + P2; третий — 1 P1 (отправка при ACL reload). Финальное ревью диапазона `96bdb6d..d0a6f50` — PASS.
- Остаются P2: fallback при возврате отозванного канала, второй `channels.changed` во время reload, мобильный вход, мёртвый R04 inbox popup, cursor/retry старой страницы, favorites-поверхность W08b, API-тесты wire-контракта.
- Не проверено: живой backend/WS/POST, два физических устройства, block/privacy на реальном сервере, ручной GUI/DPI/узкие окна и кадры web ↔ native; это остаётся для стенда и `W13`.
- Интеграция: PR #27 влит в `dev` коммитом `df306fc`; W05a — `6345627`, W08a — `d0a6f50`.

### W08a P2 — полировка «Сообщений» и ACL-сценариев — PR #31

Ветка `codex/w08a-p2` от `df306fc`, итог `f1ebb5a`. Проверки: fmt; `cargo test --workspace --locked` — 852 passed, 0 failed, 13 ignored (API 14+8+1, media 199/12 ignored, app 550/1 ignored, release 3, update 72, updater 5); release; лимиты файлов 286, `messages_ops.rs` 1193 с исключением 1200.

- Возврат «К каналам» во время ACL-reload: fallback на первый читаемый канал, сохранение черновика канала, очистка pending после ошибки reload.
- Второй `channels.changed` во время reload перезапускает запрос; мобильная нижняя панель входит/выходит из «Сообщений».
- Удалён недостижимый R04 inbox popup (`Inbox`, tiles, overlay, `show_inbox`); общие `conversation_title/peer` оставлены.
- Старая страница индекса: retry и отбрасывание ответа при смене cursor/параллельном refresh.
- 403/404 после «+»/переключения и reaction 403 проверяют беседу точечным `GET`; удалённая неактивная беседа очищает draft/pending send.
- F5 в ЛС обновляет историю; ACL-баннер только на канале; leave применяется после переключения; cap поиска и текст пустого состояния исправлены.
- Добавлены wire-тесты `crates/voxhold-api/tests/conversations_wire.rs`; P1 по потере черновика при pending-возврате исправлен в `f1ebb5a`.
- Ревью P2-диапазона — PASS с одним старым P1; follow-up `f1ebb5a` — финальное ревью PASS, P0/P1 нет.
- Остаётся P2: leave беседы, от которой переключились, не очищает её draft; тест `conversation_peer` для группы слабый; `channels.changed` без активного reload и без `voice_unconfirmed` не запускает reload.
- Не проверено: живой backend/WS/POST, block/privacy/leave на реальном сервере, ручной GUI/DPI/мобильное устройство, кадры web ↔ native; общий `C:/vxt` требует `cargo clean -p voxhold-api -p voxhold-native-rust` при параллельных worktree.

### Viewer stall recovery — PR #36

Ветка `codex/viewer-stall-recovery`; эталон backend `7f2a418`, браузерный `mediaRecovery.ts` (`DecodeWatchdog`). Проверки: `cargo test -p voxhold-media --locked viewer`; `cargo test -p voxhold-media --locked`; `cargo check --workspace --locked`; `cargo fmt --all -- --check`.

- Статичный экран на `connected`-транспорте сохраняется: неизменные `bytesReceived`/`framesDecoded` при уже декодированном кадре сбрасывают окно простоя и не запускают keyframe/ICE/re-watch. Молчание RTP больше не эскалируется в `Slow::Transport`; потерю транспорта сообщает только `on_engine_state("disconnected"/"failed")`.
- Каскад: keyframe (2 на попытку) → re-watch той же публикации с новым attempt id → ошибка после `MAX_RETRIES`. Ошибка с id keyframe-запроса не завершает просмотр. Бюджет сбрасывает только подтверждённое здоровое окно (`connected`, не recovering, нет deadline), поэтому растущая статистика без `on_frame` исчерпывает deadline.
- `transport_ended` с сохранённым intent сбрасывает `retries`; повторный `disconnected`/`failed` при вооружённом deadline считается одним retry. `missing_windows` после 6 окон без отчёта запускает существующий ICE-путь.
- `reset_for_new_engine` перенесён в `install_stream` — единственную точку установки движка; тесты проверяют совпадение `Start.attempt` с `watching().attempt`, штамп `VideoStats` и замену движка с тем же sequence/размером.
- Не проверено: реальный сервер/Pion, браузерный зритель, TURN, живые источник/энкодер; пороги 2 с/4 с/6 окон требуют замеров на реальных источниках.
