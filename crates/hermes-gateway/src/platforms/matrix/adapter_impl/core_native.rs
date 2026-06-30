impl MatrixAdapter {
    pub fn new(config: MatrixConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.access_token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let decrypt_ffi = MatrixDecryptFfiConfig::from_env();
        let native_decrypt = MatrixNativeDecryptConfig::from_env();
        if let Some(cfg) = &decrypt_ffi {
            info!(
                command = %cfg.command,
                args_len = cfg.args.len(),
                timeout_ms = cfg.timeout.as_millis() as u64,
                "Matrix decrypt FFI bridge enabled"
            );
        }
        if let Some(cfg) = &native_decrypt {
            info!(
                device_id_override = ?cfg.device_id_override,
                "Matrix native decrypt path enabled"
            );
        }
        Ok(Self {
            base,
            e2ee: MatrixE2ee::new(
                client.clone(),
                config.homeserver_url.clone(),
                config.access_token.clone(),
                config.user_id.clone(),
            ),
            config,
            client,
            txn_counter: AtomicU64::new(0),
            stop_signal: Arc::new(Notify::new()),
            sync_running: AtomicBool::new(false),
            pending_invite_joins: Arc::new(Mutex::new(HashSet::new())),
            decrypt_ffi,
            native_decrypt,
            native_runtime: AsyncMutex::new(None),
        })
    }

    pub fn config(&self) -> &MatrixConfig {
        &self.config
    }

    fn next_txn_id(&self) -> String {
        let n = self.txn_counter.fetch_add(1, Ordering::SeqCst);
        format!("hermes-{}-{}", chrono::Utc::now().timestamp_millis(), n)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.config.access_token)
    }

    fn supported_versions() -> Cow<'static, SupportedVersions> {
        Cow::Owned(SupportedVersions {
            versions: [MatrixVersion::V1_1].into(),
            features: Default::default(),
        })
    }

    fn build_ruma_request<R>(
        &self,
        request: R,
        operation: &str,
    ) -> Result<http::Request<Vec<u8>>, GatewayError>
    where
        R: OutgoingRequest + Clone,
        for<'a> R::Authentication:
            ruma::api::auth_scheme::AuthScheme<Input<'a> = SendAccessToken<'a>>,
        for<'a> R::PathBuilder:
            ruma::api::path_builder::PathBuilder<Input<'a> = Cow<'a, SupportedVersions>>,
    {
        request
            .try_into_http_request::<Vec<u8>>(
                &self.config.homeserver_url,
                SendAccessToken::Always(&self.config.access_token),
                Self::supported_versions(),
            )
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt {operation} build request failed: {e}"
                ))
            })
    }

    async fn execute_ruma_request(
        &self,
        request: http::Request<Vec<u8>>,
        operation: &str,
    ) -> Result<http::Response<Vec<u8>>, GatewayError> {
        let (parts, body) = request.into_parts();
        let method =
            reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt {operation} invalid HTTP method: {e}"
                ))
            })?;
        let uri = parts.uri.to_string();
        let url = reqwest::Url::parse(&uri).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} invalid request URI: {e}"
            ))
        })?;

        let mut req = self.client.request(method, url);
        for (name, value) in &parts.headers {
            req = req.header(name, value);
        }
        if !body.is_empty() {
            req = req.body(body);
        }

        let response = req.send().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} request failed: {e}"
            ))
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} read response failed: {e}"
            ))
        })?;

        let mut response_builder = http::Response::builder().status(status.as_u16());
        for (name, value) in &headers {
            response_builder = response_builder.header(name, value);
        }
        response_builder.body(bytes.to_vec()).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} build response failed: {e}"
            ))
        })
    }

    fn parse_ruma_response<R>(
        response: http::Response<Vec<u8>>,
        operation: &str,
    ) -> Result<R, GatewayError>
    where
        R: IncomingResponse,
        R::EndpointError: std::fmt::Debug,
    {
        R::try_from_http_response(response).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} response parse failed: {e:?}"
            ))
        })
    }

    async fn ensure_native_runtime(
        &self,
    ) -> Result<Option<Arc<MatrixNativeDecryptRuntime>>, GatewayError> {
        let Some(cfg) = self.native_decrypt.as_ref() else {
            return Ok(None);
        };

        {
            let guard = self.native_runtime.lock().await;
            if let Some(runtime) = guard.as_ref() {
                return Ok(Some(runtime.clone()));
            }
        }

        let user_id = self
            .config
            .user_id
            .parse::<ruma::OwnedUserId>()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid Matrix user_id: {e}")))?;
        let device_id = match cfg.device_id_override.as_ref() {
            Some(v) => v.clone(),
            None => self.fetch_whoami_device_id().await?,
        };
        let machine = OlmMachine::new(user_id.as_ref(), device_id.as_str().into()).await;
        let runtime = Arc::new(MatrixNativeDecryptRuntime::new(machine));

        let mut guard = self.native_runtime.lock().await;
        if let Some(existing) = guard.as_ref() {
            Ok(Some(existing.clone()))
        } else {
            *guard = Some(runtime.clone());
            Ok(Some(runtime))
        }
    }

    async fn fetch_whoami_device_id(&self) -> Result<String, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/account/whoami",
            self.config.homeserver_url
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix whoami failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix whoami error ({status}): {body}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix whoami parse: {e}")))?;

        if let Some(whoami_user) = body.get("user_id").and_then(|v| v.as_str()) {
            if whoami_user != self.config.user_id {
                warn!(
                    config_user = %self.config.user_id,
                    whoami_user,
                    "Matrix user_id differs from whoami response"
                );
            }
        }

        body.get("device_id")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(String::from)
            .ok_or_else(|| {
                GatewayError::ConnectionFailed(
                    "Matrix whoami did not return device_id; set HERMES_MATRIX_DEVICE_ID"
                        .to_string(),
                )
            })
    }

    async fn process_native_outgoing_requests(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
    ) -> Result<(), GatewayError> {
        let _request_guard = runtime.outgoing_lock.lock().await;
        loop {
            let requests = runtime.machine.outgoing_requests().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt outgoing requests failed: {e}"
                ))
            })?;
            if requests.is_empty() {
                break;
            }

            for request in requests {
                self.send_native_outgoing_request(runtime, request).await?;
            }
        }
        Ok(())
    }

    async fn send_native_outgoing_request(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request: matrix_sdk_crypto::types::requests::OutgoingRequest,
    ) -> Result<(), GatewayError> {
        let request_id = request.request_id();
        match request.request() {
            AnyOutgoingRequest::KeysUpload(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/upload")?;
                let response = self.execute_ruma_request(request, "keys/upload").await?;
                let body: upload_keys::v3::Response =
                    Self::parse_ruma_response(response, "keys/upload")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark keys/upload sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::KeysQuery(req) => {
                self.send_native_keys_query(runtime, request_id, req)
                    .await?;
            }
            AnyOutgoingRequest::KeysClaim(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/claim")?;
                let response = self.execute_ruma_request(request, "keys/claim").await?;
                let body: claim_keys::v3::Response =
                    Self::parse_ruma_response(response, "keys/claim")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark keys/claim sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::ToDeviceRequest(req) => {
                self.send_native_to_device(runtime, request_id, req).await?;
            }
            AnyOutgoingRequest::SignatureUpload(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/signatures/upload")?;
                let response = self
                    .execute_ruma_request(request, "keys/signatures/upload")
                    .await?;
                let body: upload_signatures::v3::Response =
                    Self::parse_ruma_response(response, "keys/signatures/upload")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark signatures/upload sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::RoomMessage(req) => {
                let request = send_message_event::v3::Request::new(
                    req.room_id.clone(),
                    req.txn_id.clone(),
                    req.content.as_ref(),
                )
                .map_err(|e| {
                    GatewayError::ConnectionFailed(format!(
                        "Matrix native decrypt room message request build failed: {e}"
                    ))
                })?;
                let request = self.build_ruma_request(request, "rooms/send")?;
                let response = self.execute_ruma_request(request, "rooms/send").await?;
                let body: send_message_event::v3::Response =
                    Self::parse_ruma_response(response, "rooms/send")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark room message sent failed: {e}"
                        ))
                    })?;
            }
        }

        Ok(())
    }

    async fn send_native_keys_query(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request_id: &ruma::TransactionId,
        req: &KeysQueryRequest,
    ) -> Result<(), GatewayError> {
        let mut request = get_keys::v3::Request::new();
        request.timeout = req.timeout;
        request.device_keys = req.device_keys.clone();

        let request = self.build_ruma_request(request, "keys/query")?;
        let response = self.execute_ruma_request(request, "keys/query").await?;
        let body: get_keys::v3::Response = Self::parse_ruma_response(response, "keys/query")?;
        runtime
            .machine
            .mark_request_as_sent(request_id, &body)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt mark keys/query sent failed: {e}"
                ))
            })?;
        Ok(())
    }

    async fn send_native_to_device(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request_id: &ruma::TransactionId,
        req: &ToDeviceRequest,
    ) -> Result<(), GatewayError> {
        let request = send_event_to_device::v3::Request::new_raw(
            req.event_type.clone(),
            req.txn_id.clone(),
            req.messages.clone(),
        );
        let request = self.build_ruma_request(request, "sendToDevice")?;
        let response = self.execute_ruma_request(request, "sendToDevice").await?;
        let body: send_event_to_device::v3::Response =
            Self::parse_ruma_response(response, "sendToDevice")?;
        runtime
            .machine
            .mark_request_as_sent(request_id, &body)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt mark sendToDevice sent failed: {e}"
                ))
            })?;
        Ok(())
    }

    async fn process_native_sync_changes(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        body: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let to_device_events: Vec<Raw<AnyToDeviceEvent>> = body
            .get("to_device")
            .and_then(|v| v.get("events"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt to-device parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let device_lists: DeviceLists = body
            .get("device_lists")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt device_lists parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let one_time_keys_counts: BTreeMap<OneTimeKeyAlgorithm, UInt> = body
            .get("device_one_time_keys_count")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt one-time key count parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let unused_fallback_keys: Option<Vec<OneTimeKeyAlgorithm>> = body
            .get("device_unused_fallback_key_types")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt fallback key parse failed: {e}"
                ))
            })?;

        let next_batch_token = body
            .get("next_batch")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        let sync_changes = EncryptionSyncChanges {
            to_device_events,
            changed_devices: &device_lists,
            one_time_keys_counts: &one_time_keys_counts,
            unused_fallback_keys: unused_fallback_keys.as_deref(),
            next_batch_token,
        };

        let (to_device, room_key_updates) = runtime
            .machine
            .receive_sync_changes(sync_changes, &runtime.decryption_settings)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt receive_sync_changes failed: {e}"
                ))
            })?;
        debug!(
            to_device_events = to_device.len(),
            room_key_updates = room_key_updates.len(),
            "Processed Matrix native crypto sync changes"
        );
        Ok(())
    }

    async fn try_native_decrypt_event(
        &self,
        room_id: &str,
        event: &serde_json::Value,
    ) -> Result<Option<MatrixDecryptFfiOutput>, GatewayError> {
        let Some(runtime) = self.ensure_native_runtime().await? else {
            return Ok(None);
        };

        let room_id = room_id.parse::<ruma::OwnedRoomId>().map_err(|e| {
            GatewayError::ConnectionFailed(format!("Invalid Matrix room_id for decrypt: {e}"))
        })?;
        let raw_event: Raw<EncryptedEvent> =
            serde_json::from_value(event.clone()).map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt failed to parse encrypted event: {e}"
                ))
            })?;

        let decrypted = runtime
            .machine
            .try_decrypt_room_event(&raw_event, room_id.as_ref(), &runtime.decryption_settings)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt room event failed: {e}"
                ))
            })?;

        let matrix_sdk_crypto::RoomEventDecryptionResult::Decrypted(decrypted) = decrypted else {
            return Ok(None);
        };

        let decrypted_json: serde_json::Value = serde_json::from_str(decrypted.event.json().get())
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt returned invalid JSON payload: {e}"
                ))
            })?;

        let out = Self::parse_decrypt_ffi_output(&decrypted_json.to_string()).map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix native decrypt parse: {e}"))
        })?;
        Ok(Some(out))
    }

    // -----------------------------------------------------------------------
    // Messaging
    // -----------------------------------------------------------------------

}
