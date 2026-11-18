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

Третий раунд — [`4057c7a`](https://github.com/LoonEman1/voxhold-native-rust/commit/4057c7a): отзыв доступа в ЛС сбрасывает подтверждённый поисковый снимок и старые страницы, а при закрытом popup свежий `/channels` запускается следующим открытием; `403` без окна подтверждает права только через владельца слота снимка. Выбор канала переустанавливает уже подтверждённые права до загрузки закрепов и дозагружает их после нового подтверждения. Подтверждение POST ветки снимает «Отправка…» и очищает/сохраняет живой черновик по `plan.key` даже после смены канала, а вставку строки и read ограничивает `access_generation`. «К последним» заменяет старое окно, но возвращает комментарии, пришедшие по WS во время GET, и соблюдает tombstone. После правок: `cargo fmt --all -- --check`, `cargo test --workspace --locked` (173 теста приложения: 172 passed, 1 `ignored`; 6 API; 6 media), размеры (137 файлов). Release, GUI и живой backend после правок не перезапускались; осталось: два аккаунта, кадры/DPI, живые новостные роли и закрепы/реакции ЛС, E2EE. См. [status.md](status.md).

Дополнение третьего раунда — [`0b57d56`](https://github.com/LoonEman1/voxhold-native-rust/commit/0b57d56): `channels.changed` в ЛС освобождает и слот `/channels`-проверки после `403`, поэтому старый ответ не может снова разрешить поиск; запрос проверки переведён на tokio без блокировки UI. Возврат ЛС → канал при неподтверждённом снимке запускает свежий `/channels`, и только его callback синхронизирует права и загружает закрепы. Добавлены регрессии на удержание обоих GET и на DM → канал с закрепом 42. Проверки: `cargo fmt --all -- --check`, `cargo test --workspace --locked` (173 теста приложения: 172 passed, 1 `ignored`; 6 API; 6 media), размеры (137 файлов). Release, GUI и живой backend не перезапускались.

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
