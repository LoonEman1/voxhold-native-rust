//! Reaction catalog and picker ported from Go `internal/workspace/reaction_picker.go`.
//!
//! The catalog keeps the original Unicode sequences: the value is sent to the
//! server unchanged, while the Russian label is only used by the search.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use gpui::{div, prelude::*, px, Context, Entity, Render, ScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState, TextInput};

use crate::ink;

pub const QUICK_REACTIONS: [&str; 7] = ["❤️", "😂", "👍", "🔥", "🤔", "👏", "😭"];
pub const RECENT_LIMIT: usize = 24;
/// Windows ships the color glyph table in this family; GPUI's DirectWrite
/// backend paints it as emoji instead of a monochrome fallback.
pub const EMOJI_FONT: &str = "Segoe UI Emoji";

pub struct Category {
    #[allow(dead_code)]
    pub label: &'static str,
    pub icon: &'static str,
    pub data: &'static str,
}

/// The five visible categories plus "Все" and "Недавние", exactly as in Go.
pub const CATEGORIES: [Category; 6] = [
    Category {
        label: "Все",
        icon: "✨",
        data: "",
    },
    Category {
        label: "Недавние",
        icon: "clock",
        data: "",
    },
    Category {
        label: "Лица",
        icon: "🙂",
        data: "😂:Смех|🤣:Хохот|🤔:Думаю|😁:Улыбка|😢:Грустно|😭:Плачу|😮:Удивление|😱:Шок|🤯:Взрыв мозга|🥰:Влюблённость|😍:Восторг|😎:Круто|🤩:Восхищение|🥳:Праздник|😇:Ангел|🙂:Улыбаюсь|🙃:Вверх ногами|😉:Подмигивание|😊:Радость|😋:Вкусно|😘:Поцелуй|😴:Сплю|🤤:Слюнки|🤨:Скепсис|😐:Без эмоций|😑:Без слов|😏:Усмешка|😒:Недовольство|😔:Печаль|😕:Растерянность|🙁:Огорчение|☹️:Хмурюсь|😫:Усталость|😩:Изнеможение|😤:Возмущение|😡:Злость|🤬:Ярость|😳:Смущение|🥺:Пожалуйста|🥹:Тронут|🤗:Обнимаю|🤫:Тише|🤭:Смущённый смех|🫡:Есть|🫠:Таю|🤡:Клоун|💀:Череп|👻:Призрак|👀:Смотрю|🧠:Мозг",
    },
    Category {
        label: "Жесты",
        icon: "👍",
        data: "👍:Нравится|👎:Не нравится|🤝:Согласие|👏:Аплодисменты|🙌:Ура|👐:Открытые ладони|🤲:Ладони|🙏:Спасибо|💪:Сила|✊:Кулак|👊:Дай кулачок|✌️:Победа|🤞:Удачи|🤟:Люблю|🤘:Рок|👌:Отлично|🤌:Превосходно|🫶:Сердце руками|🫰:Маленькое сердце|👋:Привет",
    },
    Category {
        label: "Праздник",
        icon: "🎉",
        data: "❤️:Любовь|🔥:Огонь|💯:Сто процентов|✅:Готово|❌:Нет|⚡:Молния|⭐:Звезда|🌟:Яркая звезда|🎉:Поздравляю|🎊:Конфетти|🎁:Подарок|💎:Бриллиант|🏆:Кубок|🥇:Первое место|🚀:Ракета",
    },
    Category {
        label: "Природа",
        icon: "🌿",
        data: "🌈:Радуга|☀️:Солнце|🌙:Луна|☕:Кофе|🍿:Попкорн|🍰:Торт|🍕:Пицца|🍓:Клубника|🍉:Арбуз|🌹:Роза|💐:Букет|🦋:Бабочка|🐱:Кот|🐶:Собака|🐸:Лягушка|🦄:Единорог|🐝:Пчела",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionOption {
    pub value: &'static str,
    pub label: &'static str,
    pub category: usize,
}

/// Parsed catalog, built once from the Go data strings.
pub fn all() -> &'static [ReactionOption] {
    static OPTIONS: OnceLock<Vec<ReactionOption>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut options = Vec::new();
        for (index, group) in CATEGORIES.iter().enumerate() {
            if group.data.is_empty() {
                continue;
            }
            for item in group.data.split('|') {
                let (value, label) = item.split_once(':').unwrap_or((item, item));
                options.push(ReactionOption {
                    value,
                    label,
                    category: index,
                });
            }
        }
        options
    })
}

/// `category` follows the Go index: 0 is all, 1 is recent, the rest are the
/// concrete groups. Search is case-insensitive over both the Russian label and
/// the emoji sequence.
pub fn filter(category: usize, query: &str, recent: &[String]) -> Vec<ReactionOption> {
    let query = query.trim().to_lowercase();
    let matches = |option: &ReactionOption| {
        query.is_empty()
            || option.label.to_lowercase().contains(&query)
            || option.value.contains(&query)
    };
    if category == 1 {
        let mut filtered = Vec::new();
        for value in recent {
            if let Some(option) = all()
                .iter()
                .find(|option| option.value == value && matches(option))
            {
                filtered.push(option.clone());
            }
        }
        return filtered;
    }
    all()
        .iter()
        .filter(|option| (category == 0 || category == option.category) && matches(option))
        .cloned()
        .collect()
}

/// Go keeps the most recent distinct reaction first and keeps 24 entries.
pub fn touch_recent(recent: &mut Vec<String>, value: &str) {
    recent.retain(|previous| previous != value);
    recent.insert(0, value.to_string());
    recent.truncate(RECENT_LIMIT);
}

pub type SharedRecents = Rc<RefCell<Vec<String>>>;

pub type PickHandler = Box<dyn Fn(&str, &mut gpui::App) + 'static>;

pub struct ReactionPicker {
    search: Entity<InputState>,
    pub recents: SharedRecents,
    pub category: usize,
    pub hover: Option<usize>,
    pub mine: std::collections::HashSet<String>,
    pub on_pick: Option<PickHandler>,
    scroll: ScrollHandle,
    _subscription: gpui::Subscription,
}

impl ReactionPicker {
    pub fn new(recents: SharedRecents, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Найти эмодзи")
                .clean_on_escape()
        });
        let subscription = cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                this.hover = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick_current(cx),
            _ => {}
        });
        Self {
            search,
            recents,
            category: 0,
            hover: None,
            mine: Default::default(),
            on_pick: None,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    pub fn reset_for_message(
        &mut self,
        mine: impl IntoIterator<Item = String>,
        cx: &mut Context<Self>,
    ) {
        self.mine = mine.into_iter().collect();
        self.category = 0;
        self.hover = None;
        self.scroll = ScrollHandle::new();
        cx.notify();
    }

    fn pick(&mut self, value: &str, cx: &mut gpui::App) {
        if let Some(handler) = &self.on_pick {
            handler(value, cx);
        }
    }

    fn pick_current(&mut self, cx: &mut gpui::App) {
        let query = self.query(cx);
        let recents = self.recents.borrow().clone();
        let options = filter(self.category, &query, &recents);
        let index = self.hover.unwrap_or(0);
        if let Some(option) = options.get(index) {
            let value = option.value;
            self.pick(value, cx);
        }
    }

    fn query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).value().to_string()
    }
}

impl Render for ReactionPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::dark_palette();
        let query = self.query(cx);
        let recents = self.recents.borrow().clone();
        let options = filter(self.category, &query, &recents);
        if let Some(hover) = self.hover.filter(|hover| *hover >= options.len()) {
            self.hover = Some(hover.min(options.len().saturating_sub(1)));
        }

        let mut categories = div().flex().w_full().h(px(31.)).gap(px(3.));
        for (index, category) in CATEGORIES.iter().enumerate() {
            let selected = index == self.category;
            let icon = category.icon;
            let is_clock = icon == "clock";
            categories = categories.child(
                div()
                    .id(("reaction-category", index as u64))
                    .relative()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .bg(if selected {
                        ink(p.selection)
                    } else {
                        gpui::rgba(0)
                    })
                    .cursor_pointer()
                    .text_size(px(18.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.category = index;
                        this.hover = None;
                        this.scroll = ScrollHandle::new();
                        cx.notify();
                    }))
                    .when(is_clock, |button| {
                        button.child(crate::icon("icons/clock.svg", 15., ink(p.muted)))
                    })
                    .when(!is_clock, |button| {
                        button.child(div().font_family(EMOJI_FONT).text_size(px(18.)).child(icon))
                    }),
            );
        }

        let mut quick = div().flex().w_full().items_center().gap(px(2.));
        for (index, value) in QUICK_REACTIONS.into_iter().enumerate() {
            let selected = self.mine.contains(value);
            quick = quick.child(
                div()
                    .id(("quick-reaction", index as u64))
                    .w(px(32.))
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(if selected {
                        ink(p.selection)
                    } else {
                        gpui::rgba(0)
                    })
                    .text_size(px(18.))
                    .font_family(EMOJI_FONT)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick(value, cx);
                        cx.stop_propagation();
                    }))
                    .child(value),
            );
        }

        let mut grid = div()
            .id("reaction-grid")
            .w_full()
            .h(px(180.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_wrap()
            .gap(px(1.))
            .pt(px(4.));
        if options.is_empty() {
            let label = if self.category == 1 && query.is_empty() {
                "Здесь появятся недавние реакции"
            } else {
                "Ничего не нашли"
            };
            grid = grid.child(
                div()
                    .p_1()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(label),
            );
        }
        for (index, option) in options.iter().enumerate() {
            let value = option.value;
            let own = self.mine.contains(value);
            let hovered = self.hover == Some(index);
            grid = grid.child(
                div()
                    .id(("reaction", index as u64))
                    .w(px(35.))
                    .h(px(35.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(if own || hovered {
                        ink(p.selection)
                    } else {
                        gpui::rgba(0)
                    })
                    .text_size(px(19.))
                    .font_family(EMOJI_FONT)
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        let next = if *hovered { Some(index) } else { None };
                        if this.hover != next {
                            this.hover = next;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick(value, cx);
                        cx.stop_propagation();
                    }))
                    .child(value),
            );
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pb(px(18.))
            .child(
                div()
                    .h(px(35.))
                    .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        let key = &event.keystroke;
                        if key.key == "enter"
                            && !key.modifiers.shift
                            && !key.modifiers.control
                            && !key.modifiers.alt
                            && !key.modifiers.platform
                        {
                            // The pinned gpui-component inserts a newline
                            // into a single-line field and GPUI panics while
                            // shaping it; Enter belongs to the picker.
                            this.pick_current(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(TextInput::new(&self.search)),
            )
            .child(categories)
            .child(div().h(px(6.)))
            .child(quick)
            .child(grid)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_the_go_reaction_picker() {
        assert_eq!(QUICK_REACTIONS, ["❤️", "😂", "👍", "🔥", "🤔", "👏", "😭"]);
        assert_eq!(CATEGORIES.len(), 6);
        assert_eq!(CATEGORIES[2].label, "Лица");
        assert_eq!(CATEGORIES[5].label, "Природа");
        assert_eq!(CATEGORIES[2].data.split('|').count(), 50);
        assert_eq!(all().len(), 50 + 20 + 15 + 17);
        assert_eq!(all()[0].value, "😂");
        assert_eq!(all()[0].label, "Смех");
    }

    #[test]
    fn search_matches_russian_labels_and_emoji() {
        let faces = filter(2, "смех", &[]);
        assert_eq!(faces.len(), 2, "Смех and Смущённый смех both match");
        assert_eq!(faces[0].value, "😂");
        assert_eq!(faces[1].value, "🤭");
        assert_eq!(filter(0, "❤", &[])[0].value, "❤️");
        assert_eq!(filter(3, "Нравится", &[])[0].value, "👍");
        assert!(filter(1, "", &[]).is_empty());
    }

    #[test]
    fn recents_stay_ordered_unique_and_bounded() {
        let mut recents = Vec::new();
        touch_recent(&mut recents, "🔥");
        touch_recent(&mut recents, "😂");
        touch_recent(&mut recents, "🔥");
        assert_eq!(recents, ["🔥", "😂"]);
        for index in 0..40 {
            touch_recent(&mut recents, &format!("{index}"));
        }
        assert_eq!(recents.len(), RECENT_LIMIT);
        assert_eq!(recents[0], "39");
        assert!(all().iter().all(|option| option.label != "39"));
    }

    #[gpui::test]
    async fn picker_renders_and_enter_picks_the_filtered_reaction(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let recents: SharedRecents = Rc::new(RefCell::new(vec!["🔥".into()]));
        let picked: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let picked_sink = picked.clone();
        let slot: Rc<RefCell<Option<Entity<ReactionPicker>>>> = Rc::new(RefCell::new(None));
        let captured = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let picker = cx.new(|cx| ReactionPicker::new(recents, window, cx));
            let sink = picked_sink;
            picker.update(cx, |picker, _| {
                picker.on_pick = Some(Box::new(move |value, _| {
                    *sink.borrow_mut() = Some(value.to_string());
                }));
            });
            *captured.borrow_mut() = Some(picker.clone());
            gpui_component::Root::new(picker.into(), window, cx)
        });
        let picker = slot.borrow().clone().expect("picker");
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, app| {
            window.draw(app).clear();
            picker.update(app, |picker, cx| {
                picker
                    .search
                    .update(cx, |state, cx| state.focus(window, cx));
            });
        });
        visual.simulate_input("смех");
        visual.simulate_keystrokes("enter");
        visual.update(|window, app| {
            window.draw(app).clear();
        });
        assert_eq!(picked.borrow().as_deref(), Some("😂"));
    }

    #[test]
    fn composed_emoji_sequences_are_preserved_byte_for_byte() {
        // Skin tone, ZWJ family, flag and variation selector stay unchanged in
        // the recent list; catalog hits also survive filtering.
        let values = ["👍🏽", "👨‍👩‍👧‍👦", "🇷🇺", "❤️"];
        for value in values {
            let mut recents = Vec::new();
            touch_recent(&mut recents, value);
            assert_eq!(recents[0], value);
            if all().iter().any(|option| option.value == value) {
                assert_eq!(filter(1, value, &recents)[0].value, value);
            }
        }
    }
}
