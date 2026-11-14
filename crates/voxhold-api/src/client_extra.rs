use super::*;

impl Client {
    pub async fn edit_channel(&self, s: i64, c: i64, name: &str) -> Result<()> {
        self.action(
            Method::PATCH,
            &channel_path(s, c),
            Some(json!({"name":name})),
        )
        .await
    }
    pub async fn delete_channel(&self, s: i64, c: i64) -> Result<()> {
        self.action(Method::DELETE, &channel_path(s, c), None).await
    }
    pub async fn channel_notifications(&self, s: i64, c: i64) -> Result<String> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/notifications", channel_path(s, c)),
                None,
            )
            .await?;
        Ok(v["mode"].as_str().unwrap_or_default().to_owned())
    }
    pub async fn set_channel_notifications(&self, s: i64, c: i64, mode: &str) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/notifications", channel_path(s, c)),
            Some(json!({"mode":mode})),
        )
        .await
    }
    pub async fn update_member_role(&self, s: i64, u: i64, role: &str) -> Result<Member> {
        self.request(
            Method::PATCH,
            &format!("/api/v1/servers/{s}/members/{u}/role"),
            Some(json!({"role":role})),
        )
        .await
    }
    pub async fn ban_member(&self, s: i64, u: i64) -> Result<()> {
        self.action(Method::POST, &format!("/api/v1/servers/{s}/bans/{u}"), None)
            .await
    }
    pub async fn set_reaction(
        &self,
        s: i64,
        c: i64,
        id: i64,
        emoji: &str,
        active: bool,
    ) -> Result<ReactionsUpdate> {
        let segment: String = url::form_urlencoded::byte_serialize(emoji.as_bytes()).collect();
        self.request(
            if active { Method::PUT } else { Method::DELETE },
            &format!("{}/reactions/{segment}", message_path(s, c, id)),
            None,
        )
        .await
    }
    pub async fn pinned_message_list(&self, s: i64, c: i64) -> Result<Vec<PinnedMessage>> {
        self.request(Method::GET, &format!("{}/pins", channel_path(s, c)), None)
            .await
    }
    pub async fn set_message_pin(&self, s: i64, c: i64, id: i64, pinned: bool) -> Result<()> {
        self.action(
            if pinned { Method::PUT } else { Method::DELETE },
            &format!("{}/pin", message_path(s, c, id)),
            None,
        )
        .await
    }
    pub async fn message_context(
        &self,
        s: i64,
        c: i64,
        id: i64,
        before: i32,
        after: i32,
    ) -> Result<MessageContext> {
        let v: MessageContext = self
            .request(
                Method::GET,
                &format!(
                    "{}/context?before={before}&after={after}",
                    message_path(s, c, id)
                ),
                None,
            )
            .await?;
        if v.target_message_id != id
            || v.messages.get(v.target_index).is_none_or(|m| m.id != id)
            || v.messages.iter().any(|m| m.channel_id != c)
            || v.messages.windows(2).any(|w| w[0].id >= w[1].id)
        {
            return Err(Error::InvalidResponse("Некорректный ответ истории."));
        }
        Ok(v)
    }
    pub async fn send_attached_message(
        &self,
        s: i64,
        c: i64,
        content: &str,
        key: &str,
        ids: &[String],
        reply: i64,
    ) -> Result<Message> {
        let mut v = json!({"content":content,"client_message_id":key,"attachment_ids":ids});
        if reply > 0 {
            v["reply_to_message_id"] = json!(reply);
        }
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(s, c)),
            Some(v),
        )
        .await
    }
    pub async fn forward_message(
        &self,
        s: i64,
        source: i64,
        id: i64,
        destination: i64,
        key: &str,
    ) -> Result<Message> {
        self.request(Method::POST,&format!("{}/messages/forward",channel_path(s,destination)),Some(json!({"source_server_id":s,"source_channel_id":source,"source_message_id":id,"client_message_id":key}))).await
    }
    pub async fn save_favorite(&self, id: i64, key: &str) -> Result<()> {
        let v: Value = self
            .request(Method::GET, "/api/v1/me/favorites", None)
            .await?;
        self.action(Method::POST,"/api/v1/me/forward",Some(json!({"source":{"kind":"channel","id":id},"destination":{"kind":"conversation","id":v["id"]},"client_message_id":key}))).await
    }
    pub async fn link_preview(&self, href: &str) -> Result<LinkPreview> {
        let q: String = url::form_urlencoded::byte_serialize(href.as_bytes()).collect();
        self.request(Method::GET, &format!("/api/v1/link-preview?url={q}"), None)
            .await
    }
    pub async fn profile(&self) -> Result<Profile> {
        self.request(Method::GET, "/api/v1/me/profile", None).await
    }
    pub async fn update_profile(&self, about: &str, country: &str) -> Result<Profile> {
        self.request(
            Method::PATCH,
            "/api/v1/me/profile",
            Some(json!({"about":about,"country_code":country})),
        )
        .await
    }
    pub async fn remove_avatar(&self) -> Result<()> {
        self.action(Method::DELETE, "/api/v1/me/avatar", None).await
    }
    pub async fn upload_avatar(
        &self,
        data: Vec<u8>,
        crop: Option<(i32, i32, i32)>,
    ) -> Result<String> {
        let mut u = self.url("/api/v1/me/avatar")?;
        if let Some((x, y, size)) = crop {
            u.query_pairs_mut()
                .append_pair("crop_x", &x.to_string())
                .append_pair("crop_y", &y.to_string())
                .append_pair("crop_size", &size.to_string());
        }
        let b = self.binary(Method::PUT, u, data, 1 << 20).await?;
        let v: Value = serde_json::from_slice(&b)?;
        Ok(v["etag"].as_str().unwrap_or_default().to_owned())
    }
    pub async fn change_password(&self, current: &str, next: &str, confirm: &str) -> Result<()> {
        self.action(Method::PUT,"/api/v1/me/password",Some(json!({"current_password":current,"new_password":next,"new_password_confirm":confirm}))).await
    }
    pub async fn sessions(&self) -> Result<Vec<AccountSession>> {
        let v: Value = self
            .request(Method::GET, "/api/v1/me/sessions", None)
            .await?;
        Ok(serde_json::from_value(v["sessions"].clone())?)
    }
    pub async fn revoke_session(&self, id: i64) -> Result<()> {
        self.action(Method::DELETE, &format!("/api/v1/me/sessions/{id}"), None)
            .await
    }
    pub async fn revoke_other_sessions(&self) -> Result<()> {
        self.action(Method::DELETE, "/api/v1/me/sessions", None)
            .await
    }
    pub async fn delete_account(&self, password: &str) -> Result<()> {
        self.action(
            Method::DELETE,
            "/api/v1/account",
            Some(json!({"password":password})),
        )
        .await
    }
    pub async fn message_privacy(&self) -> Result<MessagePrivacy> {
        self.request(Method::GET, "/api/v1/me/message-privacy", None)
            .await
    }
    pub async fn set_message_privacy(&self, mode: &str) -> Result<MessagePrivacy> {
        self.request(
            Method::PUT,
            "/api/v1/me/message-privacy",
            Some(json!({"mode":mode})),
        )
        .await
    }
    pub async fn block_user(&self, id: i64, blocked: bool) -> Result<()> {
        self.action(
            if blocked { Method::PUT } else { Method::DELETE },
            &format!("/api/v1/me/blocks/{id}"),
            None,
        )
        .await
    }
    pub async fn rename_server(&self, id: i64, name: &str) -> Result<()> {
        self.action(
            Method::PATCH,
            &format!("/api/v1/servers/{id}"),
            Some(json!({"name":name})),
        )
        .await
    }
    pub async fn channel_access(&self, s: i64, c: i64) -> Result<ChannelAccess> {
        self.request(Method::GET, &format!("{}/access", channel_path(s, c)), None)
            .await
    }
    pub async fn set_channel_access(&self, s: i64, c: i64, v: &ChannelAccess) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/access", channel_path(s, c)),
            Some(json!(v)),
        )
        .await
    }
    pub async fn news_settings(&self, s: i64, c: i64) -> Result<NewsSettings> {
        self.request(Method::GET, &format!("{}/news", channel_path(s, c)), None)
            .await
    }
    pub async fn set_news_settings(&self, s: i64, c: i64, v: &NewsSettings) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/news", channel_path(s, c)),
            Some(json!(v)),
        )
        .await
    }
    pub async fn roles(&self, s: i64) -> Result<Vec<CustomRole>> {
        self.request(Method::GET, &format!("/api/v1/servers/{s}/roles"), None)
            .await
    }
    pub async fn save_role(&self, s: i64, r: &CustomRole) -> Result<()> {
        let mut path = format!("/api/v1/servers/{s}/roles");
        let method = if r.id != 0 {
            path.push_str(&format!("/{}", r.id));
            Method::PUT
        } else {
            Method::POST
        };
        self.action(
            method,
            &path,
            Some(json!({"name":r.name,"color":r.color,"permissions":r.permissions})),
        )
        .await
    }
    pub async fn delete_role(&self, s: i64, id: i64) -> Result<()> {
        self.action(
            Method::DELETE,
            &format!("/api/v1/servers/{s}/roles/{id}"),
            None,
        )
        .await
    }
    pub async fn role_preview(&self, s: i64, id: i64) -> Result<Vec<RolePreview>> {
        self.request(
            Method::GET,
            &format!("/api/v1/servers/{s}/roles/{id}/preview"),
            None,
        )
        .await
    }
    pub async fn set_member_roles(&self, s: i64, u: i64, ids: &[i64]) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("/api/v1/servers/{s}/members/{u}/roles"),
            Some(json!({"role_ids":ids})),
        )
        .await
    }
    pub async fn role_grants(&self, s: i64, c: i64) -> Result<Vec<i64>> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/role-grants", channel_path(s, c)),
                None,
            )
            .await?;
        Ok(serde_json::from_value(v["role_ids"].clone())?)
    }
    pub async fn set_role_grants(&self, s: i64, c: i64, ids: &[i64]) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/role-grants", channel_path(s, c)),
            Some(json!({"role_ids":ids})),
        )
        .await
    }
}
impl Client {
    pub async fn client_diagnostics(&self) -> Result<Value> {
        Ok(serde_json::from_slice(
            &self
                .bytes(
                    Method::GET,
                    "/api/v1/diagnostics/client-events?limit=5000",
                    None,
                    32 << 20,
                )
                .await?,
        )?)
    }
    pub async fn cors_origins(&self) -> Result<Vec<String>> {
        let v: Value = self
            .request(Method::GET, "/api/v1/instance/cors-origins", None)
            .await?;
        Ok(serde_json::from_value(v["origins"].clone())?)
    }
    pub async fn set_cors_origins(&self, origins: &[String]) -> Result<()> {
        self.action(
            Method::PUT,
            "/api/v1/instance/cors-origins",
            Some(json!({"origins":origins})),
        )
        .await
    }
    pub async fn slowmode(&self, s: i64, c: i64) -> Result<i64> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/slowmode", channel_path(s, c)),
                None,
            )
            .await?;
        v["seconds"]
            .as_i64()
            .ok_or(Error::InvalidResponse("invalid slowmode"))
    }
    pub async fn set_slowmode(&self, s: i64, c: i64, seconds: i64) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/slowmode", channel_path(s, c)),
            Some(json!({"seconds":seconds})),
        )
        .await
    }
    pub async fn voice_encryption(&self, s: i64, c: i64) -> Result<bool> {
        let v: Value = self
            .request(Method::GET, &format!("{}/e2ee", channel_path(s, c)), None)
            .await?;
        v["required"]
            .as_bool()
            .ok_or(Error::InvalidResponse("invalid encryption state"))
    }
    pub async fn set_voice_encryption(&self, s: i64, c: i64, required: bool) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/e2ee", channel_path(s, c)),
            Some(json!({"required":required})),
        )
        .await
    }
    pub async fn storage(&self) -> Result<StorageState> {
        self.request(Method::GET, "/api/v1/instance/storage", None)
            .await
    }
    pub async fn set_storage(&self, quotas: Value) -> Result<StorageState> {
        self.request(Method::PUT, "/api/v1/instance/storage", Some(quotas))
            .await
    }
    pub async fn preview_cleanup(&self, category: &str, before: i64) -> Result<CleanupPlan> {
        self.request(
            Method::POST,
            "/api/v1/instance/storage/preview",
            Some(json!({"category":category,"before":before})),
        )
        .await
    }
    pub async fn cleanup(&self, token: &str) -> Result<CleanupPlan> {
        self.request(
            Method::POST,
            "/api/v1/instance/storage/cleanup",
            Some(json!({"token":token})),
        )
        .await
    }
    pub async fn bans(&self, s: i64) -> Result<Vec<ServerBan>> {
        let v: Value = self
            .request(Method::GET, &format!("/api/v1/servers/{s}/bans"), None)
            .await?;
        Ok(serde_json::from_value(v["bans"].clone())?)
    }
    pub async fn unban(&self, s: i64, u: i64) -> Result<()> {
        self.action(
            Method::DELETE,
            &format!("/api/v1/servers/{s}/bans/{u}"),
            None,
        )
        .await
    }
}

impl Client {
    /// Subscribe to read snapshots and updates for one channel. Dropping the
    /// future closes the socket, which allows callers to cancel on navigation.
    pub async fn read_receipts<F: FnMut(Vec<ChannelRead>)>(
        &self,
        server_id: i64,
        channel_id: i64,
        mut receive: F,
    ) -> Result<()> {
        self.subscribe(server_id, channel_id, |event| {
            if event.kind == "channel.read_snapshot" {
                let data = &event.data;
                if data["server_id"].as_i64() == Some(server_id)
                    && data["channel_id"].as_i64() == Some(channel_id)
                {
                    if let Ok(reads) = serde_json::from_value(data["reads"].clone()) {
                        receive(reads);
                    }
                }
            } else if event.kind == "channel.read" {
                if let Ok(read) = serde_json::from_value::<ChannelRead>(event.data) {
                    if read.server_id == server_id && read.channel_id == channel_id {
                        receive(vec![read]);
                    }
                }
            }
        })
        .await
    }
}
