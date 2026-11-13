use std::sync::Arc;
use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use voxhold_api::{AccountSession, Client, MessagePrivacy, Profile};

pub struct PersonalSettings {
    client: Client, runtime: Arc<tokio::runtime::Runtime>,
    about: Entity<InputState>, country: Entity<InputState>, current: Entity<InputState>, next: Entity<InputState>, confirm: Entity<InputState>, mode: Entity<InputState>, user_id: Entity<InputState>, delete_name: Entity<InputState>, delete_confirm: bool,
    profile: Option<Profile>, privacy: Option<MessagePrivacy>, sessions: Vec<AccountSession>, error: String, notice: String, busy: bool, generation: u64,
}
impl PersonalSettings {
    pub fn new(client: Client, runtime: Arc<tokio::runtime::Runtime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { client, runtime,
            about:cx.new(|cx|InputState::new(window,cx).multi_line().placeholder("О себе")),
            country:cx.new(|cx|InputState::new(window,cx).placeholder("Код страны")),
            current:cx.new(|cx|InputState::new(window,cx).masked(true).placeholder("Текущий пароль")),
            next:cx.new(|cx|InputState::new(window,cx).masked(true).placeholder("Новый пароль")),
            confirm:cx.new(|cx|InputState::new(window,cx).masked(true).placeholder("Повторите новый пароль")),
            mode:cx.new(|cx|InputState::new(window,cx).placeholder("Режим личных сообщений")),
            user_id:cx.new(|cx|InputState::new(window,cx).placeholder("ID пользователя")),
            delete_name:cx.new(|cx|InputState::new(window,cx).placeholder("Имя пользователя для удаления")), delete_confirm:false,
            profile:None,privacy:None,sessions:vec![],error:String::new(),notice:String::new(),busy:false,generation:0 }
    }
    pub fn load(&mut self, cx: &mut Context<Self>) { self.perform(Action::Load,cx); }
    fn perform(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy{return;} if matches!(action,Action::DeleteAccount) { let name=self.delete_name.read(cx).value().to_string(); if self.profile.as_ref().is_none_or(|p|p.username!=name) || self.current.read(cx).value().is_empty() {self.error="Введите имя аккаунта и текущий пароль.".into();cx.notify();return;} if !self.delete_confirm {self.delete_confirm=true;cx.notify();return;} self.delete_confirm=false; } let about=self.about.read(cx).value().to_string(); let country=self.country.read(cx).value().to_string(); let current=self.current.read(cx).value().to_string(); let next=self.next.read(cx).value().to_string(); let confirm=self.confirm.read(cx).value().to_string(); let mode=self.mode.read(cx).value().to_string(); let uid=self.user_id.read(cx).value().parse::<i64>().unwrap_or(0);
        if matches!(action,Action::Block|Action::Unblock) && uid<=0 {self.error="Укажите ID пользователя.".into();cx.notify();return;}
        self.busy=true;self.error.clear();self.notice.clear();self.generation+=1;let generation=self.generation;let client=self.client.clone();let runtime=self.runtime.clone();cx.notify();
        let task=cx.background_spawn(async move {runtime.block_on(async move {
            match action {
                Action::Load => {let profile=client.profile().await.map_err(|e|e.to_string())?; let privacy=client.message_privacy().await.map_err(|e|e.to_string())?;let sessions=client.sessions().await.map_err(|e|e.to_string())?;Ok(Response::Loaded(profile,privacy,sessions))},
                Action::Profile => client.update_profile(&about,&country).await.map(Response::Profile),
                Action::Password => client.change_password(&current,&next,&confirm).await.map(|_|Response::Done),
                Action::Privacy => client.set_message_privacy(&mode).await.map(Response::Privacy),
                Action::Block => client.block_user(uid,true).await.map(|_|Response::Done),
                Action::Unblock => client.block_user(uid,false).await.map(|_|Response::Done),
                Action::Revoke(id) => client.revoke_session(id).await.map(|_|Response::Done),
                Action::RevokeOthers => client.revoke_other_sessions().await.map(|_|Response::Done),
                Action::RemoveAvatar => client.remove_avatar().await.map(|_|Response::Done),
                Action::DeleteAccount => client.delete_account(&current).await.map(|_|Response::Done),
            }.map_err(|e:voxhold_api::Error|e.to_string())
        })});
        cx.spawn(async move |this,cx| {let result=task.await;if let Some(this)=this.upgrade(){this.update(cx,|s,cx|{if s.generation!=generation{return;}s.busy=false;match result {Ok(Response::Loaded(p,privacy,sessions))=>{s.profile=Some(p);s.privacy=Some(privacy);s.sessions=sessions},Ok(Response::Profile(p))=>s.profile=Some(p),Ok(Response::Privacy(p))=>s.privacy=Some(p),Ok(Response::Done)=>s.notice="Сохранено".into(),Err(e)=>s.error=e};cx.notify();}).ok();}}).detach();
    }
}
#[derive(Clone,Copy)] enum Action { Load,Profile,Password,Privacy,Block,Unblock,Revoke(i64),RevokeOthers,RemoveAvatar,DeleteAccount }
enum Response { Loaded(Profile,MessagePrivacy,Vec<AccountSession>),Profile(Profile),Privacy(MessagePrivacy),Done }
impl Render for PersonalSettings {
    fn render(&mut self,_:&mut Window,cx:&mut Context<Self>)->impl IntoElement {
        let mut root=div().id("personal-settings").w(px(480.)).h_full().overflow_y_scroll().p_4().flex().flex_col().gap_2().bg(rgb(0x202323)).text_color(rgb(0xf3f4eb)).child("Личные настройки");
        if let Some(p)=&self.profile {root=root.child(format!("{} · {}",p.username,p.about));}
        root=root.child(div().id("settings-refresh").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.load(cx))).child("Обновить"))
          .child(TextInput::new(&self.about)).child(TextInput::new(&self.country))
          .child(div().id("profile-save").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::Profile,cx))).child("Сохранить профиль"))
          .child(div().id("avatar-remove").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::RemoveAvatar,cx))).child("Удалить аватар"))
          .child("Пароль").child(TextInput::new(&self.current)).child(TextInput::new(&self.next)).child(TextInput::new(&self.confirm))
          .child(div().id("password-save").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::Password,cx))).child("Изменить пароль"))
          .child("Личные сообщения");
        if let Some(p)=&self.privacy {root=root.child(format!("Режим: {} · заблокированы: {:?}",p.mode,p.blocked_user_ids));}
        root=root.child(TextInput::new(&self.mode)).child(div().id("privacy-save").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::Privacy,cx))).child("Сохранить режим"))
          .child(TextInput::new(&self.user_id)).child(div().id("block").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::Block,cx))).child("Заблокировать"))
          .child(div().id("unblock").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::Unblock,cx))).child("Разблокировать"))
          .child("Сеансы");
        for session in &self.sessions {let id=session.id;root=root.child(div().flex().gap_2().child(format!("{} · {}{}",session.user_agent,session.client_ip,if session.current {" · текущий"}else{""})).child(div().id(("revoke",id as u64)).cursor_pointer().on_click(cx.listener(move |s,_,_,cx|s.perform(Action::Revoke(id),cx))).child("Завершить")));}
        root.child(div().id("revoke-others").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::RevokeOthers,cx))).child("Завершить другие сеансы"))
          .child(TextInput::new(&self.delete_name)).child(div().id("delete-account").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.perform(Action::DeleteAccount,cx))).child(if self.delete_confirm {"Подтвердить удаление аккаунта"} else {"Удалить аккаунт"}))
          .child(if self.busy {"Загрузка…"}else{""}).child(self.notice.clone()).child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
    }
}

