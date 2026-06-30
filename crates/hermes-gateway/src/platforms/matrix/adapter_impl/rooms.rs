impl MatrixAdapter {
    // -----------------------------------------------------------------------
    // Room management
    // -----------------------------------------------------------------------

    /// Join a room by room ID or alias.
    pub async fn join_room(&self, room_id: &str) -> Result<String, GatewayError> {
        Self::join_room_with_client(&self.client, &self.config, room_id).await
    }

    async fn join_room_with_client(
        client: &Client,
        config: &MatrixConfig,
        room_id: &str,
    ) -> Result<String, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/join/{}",
            config.homeserver_url, room_id
        );

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.access_token))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix join room failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix join room error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix join parse failed: {e}"))
        })?;

        let joined_room = result
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(room_id)
            .to_string();

        info!(room_id = %joined_room, "Joined room");
        Ok(joined_room)
    }

    fn schedule_invite_join(&self, invite: MatrixInviteJoinRequest) {
        let room_id = invite.room_id.trim().to_string();
        if room_id.is_empty() {
            return;
        }

        {
            let mut pending = self.pending_invite_joins.lock().expect("matrix invite joins");
            if !pending.insert(room_id.clone()) {
                debug!(room_id = %room_id, "Matrix invite join already pending");
                return;
            }
        }

        let pending = Arc::clone(&self.pending_invite_joins);
        let client = self.client.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            info!(room_id = %room_id, "Scheduling Matrix invite auto-join");
            let join_result = tokio::time::timeout(
                Duration::from_secs(45),
                Self::join_room_with_client(&client, &config, &room_id),
            )
            .await;

            match join_result {
                Ok(Ok(_joined_room)) => {
                    if invite.is_direct {
                        if let Some(inviter) =
                            invite.inviter.as_deref().map(str::trim).filter(|s| !s.is_empty())
                        {
                            if let Err(err) =
                                Self::record_dm_room_with_client(&client, &config, &room_id, inviter)
                                    .await
                            {
                                warn!(
                                    room_id = %room_id,
                                    inviter = %inviter,
                                    error = %err,
                                    "Matrix failed to record direct invite in m.direct"
                                );
                            }
                        }
                    }
                }
                Ok(Err(err)) => {
                    warn!(room_id = %room_id, error = %err, "Failed to auto-join Matrix invite");
                }
                Err(_) => {
                    warn!(room_id = %room_id, "Timed out auto-joining Matrix invite");
                }
            }

            pending
                .lock()
                .expect("matrix invite joins")
                .remove(&room_id);
        });
    }

    async fn record_dm_room_with_client(
        client: &Client,
        config: &MatrixConfig,
        room_id: &str,
        inviter: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/user/{}/account_data/m.direct",
            config.homeserver_url,
            urlencoding::encode(&config.user_id)
        );

        let mut account_data = match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", config.access_token))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Map<String, serde_json::Value>>()
                .await
                .unwrap_or_default(),
            Ok(resp) if resp.status().as_u16() == 404 => serde_json::Map::new(),
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(GatewayError::ConnectionFailed(format!(
                    "Matrix m.direct read error ({status}): {text}"
                )));
            }
            Err(err) => {
                return Err(GatewayError::ConnectionFailed(format!(
                    "Matrix m.direct read failed: {err}"
                )));
            }
        };

        let rooms = account_data
            .entry(inviter.to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if !rooms.is_array() {
            *rooms = serde_json::Value::Array(Vec::new());
        }
        let rooms = rooms.as_array_mut().expect("normalized array");
        if !rooms.iter().any(|room| room.as_str() == Some(room_id)) {
            rooms.push(serde_json::Value::String(room_id.to_string()));
        }

        let resp = client
            .put(&url)
            .header("Authorization", format!("Bearer {}", config.access_token))
            .json(&account_data)
            .send()
            .await
            .map_err(|err| {
                GatewayError::ConnectionFailed(format!("Matrix m.direct write failed: {err}"))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix m.direct write error ({status}): {text}"
            )));
        }

        info!(
            room_id = %room_id,
            inviter = %inviter,
            "Recorded Matrix direct invite in m.direct"
        );
        Ok(())
    }

    #[cfg(test)]
    fn pending_invite_join_count(&self) -> usize {
        self.pending_invite_joins
            .lock()
            .expect("matrix invite joins")
            .len()
    }

    /// Leave a room.
    pub async fn leave_room(&self, room_id: &str) -> Result<(), GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/leave",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix leave room failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix leave room error: {text}"
            )));
        }

        info!(room_id, "Left room");
        Ok(())
    }

    /// Get the list of members in a room.
    pub async fn get_room_members(&self, room_id: &str) -> Result<Vec<RoomMember>, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/members",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix get members failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix get members error: {text}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix members parse: {e}")))?;

        let mut members = Vec::new();
        if let Some(chunks) = body.get("chunk").and_then(|v| v.as_array()) {
            for event in chunks {
                let user_id = event
                    .get("state_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = event.get("content");
                let membership = content
                    .and_then(|c| c.get("membership"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("leave")
                    .to_string();
                let display_name = content
                    .and_then(|c| c.get("displayname"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                members.push(RoomMember {
                    user_id,
                    display_name,
                    membership,
                });
            }
        }

        debug!(room_id, count = members.len(), "Fetched room members");
        Ok(members)
    }

    fn room_server_name(room_id: &str) -> Option<String> {
        room_id
            .rsplit_once(':')
            .map(|(_, server)| server.trim())
            .filter(|server| !server.is_empty())
            .map(str::to_string)
    }

    pub fn classify_room_identity(
        room_id: &str,
        room_name: Option<String>,
        canonical_alias: Option<String>,
        joined_member_count: Option<usize>,
        is_direct_account_data: bool,
    ) -> MatrixRoomIdentity {
        let has_explicit_name = room_name
            .as_deref()
            .map(str::trim)
            .is_some_and(|name| !name.is_empty());
        let is_likely_dm = joined_member_count.is_some_and(|count| count <= 2)
            || (joined_member_count.is_none() && is_direct_account_data && !has_explicit_name);
        let direct_conflict = is_direct_account_data
            && has_explicit_name
            && joined_member_count.is_none_or(|n| n > 2);
        let display_name = room_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| {
                canonical_alias
                    .as_deref()
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
            })
            .unwrap_or(room_id)
            .to_string();

        MatrixRoomIdentity {
            room_id: room_id.to_string(),
            room_name,
            canonical_alias,
            server_name: Self::room_server_name(room_id),
            joined_member_count,
            is_direct_account_data,
            direct_conflict,
            chat_type: if is_likely_dm { "dm" } else { "room" }.to_string(),
            display_name,
        }
    }

    pub async fn resolve_room_identity(
        &self,
        room_id: &str,
        is_direct_account_data: bool,
    ) -> MatrixRoomIdentity {
        let room_name = match self.get_room_name(room_id).await {
            Ok(name) => name,
            Err(err) => {
                debug!(room_id, error = %err, "Matrix room name lookup unavailable");
                None
            }
        };
        let joined_member_count = match self.get_room_members(room_id).await {
            Ok(members) => Some(
                members
                    .iter()
                    .filter(|member| member.membership == "join")
                    .count(),
            ),
            Err(err) => {
                debug!(room_id, error = %err, "Matrix room member count lookup unavailable");
                None
            }
        };

        Self::classify_room_identity(
            room_id,
            room_name,
            None,
            joined_member_count,
            is_direct_account_data,
        )
    }

    /// Get the power levels for a room.
    pub async fn get_room_power_levels(
        &self,
        room_id: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.power_levels/",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix power levels failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix power levels error: {text}"
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix power levels parse: {e}"))
        })?;

        Ok(body)
    }

    /// Get the display name of a room.
    pub async fn get_room_name(&self, room_id: &str) -> Result<Option<String>, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.name/",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix room name failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix room name error: {text}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix room name parse: {e}")))?;

        Ok(body.get("name").and_then(|v| v.as_str()).map(String::from))
    }

    // -----------------------------------------------------------------------
    // Media upload
    // -----------------------------------------------------------------------

}
