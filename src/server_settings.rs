use std::sync::Arc;
use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use voxhold_api::{Client, ServerBan};

/// Server and instance settings; only the owner may open this panel.
pub struct ServerSettings {
    client:Client, runtime:Arc<tokio::runtime::Runtime>, server:i64, owner:bool,
    name:Entity<InputState>, cors:Entity<InputState>, bans:Vec<ServerBan>,
    loaded:bool, busy:bool, generation:u64, confirm_unban:Option<i64>, error:String, notice:String,
}
impl ServerSettings {
    pub fn new(client:Client,runtime:Arc<tokio::runtime::Runtime>,window:&mut Window,cx:&mut Context<Self>)->Self {
        Self {client,runtime,server:0,owner:false,name:cx.new(|cx|InputState::new(window,cx).placeholder("Название сервера")),cors:cx.new(|cx|InputState::new(window,cx).multi_line().placeholder("По одному origin на строку")),bans:vec![],loaded:false,busy:false,generation:0,confirm_unban:None,error:String::new(),notice:String::new()}
    }
    pub fn set_scope(&mut self,server:i64,name:&str,owner:bool,window:&mut Window,cx:&mut Context<Self>) {
        self.server=server;self.owner=owner;self.loaded=false;self.busy=false;self.generation+=1;self.bans.clear();self.confirm_unban=None;self.error.clear();self.notice.clear();
        self.name.update(cx,|v,cx|v.set_value(name.to_owned(),window,cx));self.cors.update(cx,|v,cx|v.set_value("",window,cx));cx.notify();
    }
    pub fn load(&mut self,window:&mut Window,cx:&mut Context<Self>) {
        if !self.owner||self.server<=0||self.busy{return;}self.busy=true;self.error.clear();let generation=self.generation;let server=self.server;let client=self.client.clone();let runtime=self.runtime.clone();cx.notify();
        let task=cx.background_spawn(async move {runtime.block_on(async move {Ok::<_,voxhold_api::Error>((client.cors_origins().await?,client.bans(server).await?))}).map_err(|e|e.to_string())});
        cx.spawn_in(window,async move |this,window|{let result=task.await;let _=this.update_in(window,|s,window,cx|{if s.generation!=generation{return;}s.busy=false;match result {Ok((cors,bans))=>{s.cors.update(cx,|v,cx|v.set_value(cors.join("\n"),window,cx));s.bans=bans;s.loaded=true},Err(e)=>s.error=e};cx.notify();});}).detach();
    }
    fn diagnostics(&mut self,cx:&mut Context<Self>) {
        if !self.owner || !self.loaded || self.busy {return;}
        let directory=std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let prompt=cx.prompt_for_new_path(&directory,Some("voxhold-diagnostics.json"));
        let generation=self.generation;
        cx.spawn(async move |this,cx| {
            let path=match prompt.await {Ok(Ok(Some(path)))=>path,_=>return};
            let Some(entity)=this.upgrade() else {return};
            let task=entity.update(cx,|s,cx| {
                if s.generation!=generation || !s.owner || s.busy {return None;}
                s.busy=true;s.error.clear();s.notice.clear();cx.notify();let client=s.client.clone();let runtime=s.runtime.clone();
                Some(cx.background_spawn(async move {let value=runtime.block_on(client.client_diagnostics()).map_err(|e|e.to_string())?;std::fs::write(path,value.to_string()).map_err(|e|e.to_string())}))
            }).ok().flatten();
            let Some(task)=task else{return}; let result=task.await;
            let _=entity.update(cx,|s,cx| {if s.generation!=generation{return;}s.busy=false;match result {Ok(())=>s.notice="Диагностика сохранена".into(),Err(e)=>s.error=e}cx.notify();});
        }).detach();
    }
    fn action(&mut self,op:Op,cx:&mut Context<Self>) {
        if !self.owner||self.server<=0||self.busy||!self.loaded{return;}
        if let Op::Unban(id)=op {if self.confirm_unban!=Some(id){self.confirm_unban=Some(id);cx.notify();return;}}
        let name=self.name.read(cx).value().trim().to_string();if matches!(op,Op::Rename)&&name.is_empty(){self.error="Введите название сервера".into();cx.notify();return;}
        let origins:Vec<String>=self.cors.read(cx).value().lines().map(str::trim).filter(|s|!s.is_empty()).map(str::to_owned).collect();
        self.busy=true;self.error.clear();self.notice.clear();self.confirm_unban=None;let generation=self.generation;let server=self.server;let client=self.client.clone();let runtime=self.runtime.clone();cx.notify();
        let task=cx.background_spawn(async move {runtime.block_on(async move {match op {Op::Rename=>client.rename_server(server,&name).await,Op::Cors=>client.set_cors_origins(&origins).await,Op::Unban(id)=>client.unban(server,id).await}.map_err(|e|e.to_string())})});
        cx.spawn(async move |this,cx|{let result=task.await;if let Some(this)=this.upgrade(){this.update(cx,|s,cx|{if s.generation!=generation{return;}s.busy=false;match result {Ok(())=>{if let Op::Unban(id)=op{s.bans.retain(|b|b.user_id!=id);}s.notice="Сохранено".into()},Err(e)=>s.error=e};cx.notify();}).ok();}}).detach();
    }
}
#[derive(Clone,Copy)] enum Op {Rename,Cors,Unban(i64)}
impl Render for ServerSettings {
    fn render(&mut self,_:&mut Window,cx:&mut Context<Self>)->impl IntoElement {
        let mut root=div().id("server-settings").w(px(440.)).h_full().overflow_y_scroll().p_4().flex().flex_col().gap_2().bg(rgb(0x202323)).text_color(rgb(0xf3f4eb)).child("Настройки сервера");
        if !self.owner{return root.child("Только владелец сервера").into_any_element();}
        if !self.loaded{return root.child(div().id("server-settings-load").cursor_pointer().on_click(cx.listener(|s,_,window,cx|s.load(window,cx))).child("Загрузить настройки")).child(self.error.clone()).into_any_element();}
        root=root.child(TextInput::new(&self.name)).child(div().id("server-rename").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.action(Op::Rename,cx))).child("Сохранить название"))
          .child("Разрешённые origin").child(TextInput::new(&self.cors)).child(div().id("cors-save").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.action(Op::Cors,cx))).child("Сохранить CORS"))
          .child(div().id("diagnostics-save").cursor_pointer().on_click(cx.listener(|s,_,_,cx|s.diagnostics(cx))).child("Скачать диагностику JSON"))
          .child("Блокировки");
        for ban in &self.bans {let id=ban.user_id;root=root.child(div().flex().gap_2().child(format!("{} · {}",ban.username,ban.reason)).child(div().id(("unban",id as u64)).cursor_pointer().on_click(cx.listener(move |s,_,_,cx|s.action(Op::Unban(id),cx))).child(if self.confirm_unban==Some(id){"Подтвердить"}else{"Разблокировать"})));}
        root.child(if self.busy {"Загрузка…"}else{""}).child(self.notice.clone()).child(div().text_color(rgb(0xff7777)).child(self.error.clone())).into_any_element()
    }
}

