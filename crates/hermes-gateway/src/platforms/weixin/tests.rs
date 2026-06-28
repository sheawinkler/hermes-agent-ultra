#[cfg(test)]
mod weixin_crypto_tests {
    use super::*;

    #[test]
    fn parse_aes_key_raw_16_bytes() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let b64 = base64::engine::general_purpose::STANDARD.encode(key);
        let out = parse_aes_key(&b64).unwrap();
        assert_eq!(out, key);
    }

    #[test]
    fn parse_aes_key_hex_payload_after_b64_decode() {
        let hex = "0123456789abcdef0123456789abcdef";
        let b64 = base64::engine::general_purpose::STANDARD.encode(hex.as_bytes());
        let out = parse_aes_key(&b64).unwrap();
        let expected: [u8; 16] = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn parse_aes_key_rejects_bad_input() {
        assert!(parse_aes_key("not-valid-base64!!!").is_err());
        assert!(parse_aes_key("Zg==").is_err());
    }

    #[test]
    fn aes128_ecb_roundtrip_short_plaintext() {
        let key = [7u8; 16];
        let plain = b"hello-weixin-ilink";
        let ct = aes128_ecb_encrypt(plain, &key);
        assert_eq!(ct.len() % 16, 0);
        let back = aes128_ecb_decrypt(&ct, &key).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn aes128_ecb_roundtrip_block_aligned_plaintext() {
        let key: [u8; 16] = [7u8; 16];
        let plain = [0xabu8; 32];
        let ct = aes128_ecb_encrypt(&plain, &key);
        let back = aes128_ecb_decrypt(&ct, &key).unwrap();
        assert_eq!(back, plain.as_slice());
    }

    #[test]
    fn aes128_ecb_decrypt_rejects_non_block_length() {
        let key = [0u8; 16];
        assert!(aes128_ecb_decrypt(&[1u8; 15], &key).is_err());
    }
}

#[cfg(test)]
mod weixin_send_file_tests {
    use super::*;
    use std::io::Write;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_cfg(base: &str) -> WeixinConfig {
        WeixinConfig {
            account_id: "acc_test".into(),
            token: "tok_test".into(),
            base_url: base.into(),
            cdn_base_url: base.into(),
            dm_policy: "open".into(),
            group_policy: "disabled".into(),
            allow_from: vec![],
            group_allow_from: vec![],
            proxy: AdapterProxyConfig::default(),
        }
    }

    #[tokio::test]
    async fn send_ilink_file_upload_param_path_end_to_end() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/getuploadurl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ret": 0,
                "upload_param": "up_param_1"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/upload"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-encrypted-param", "enc_param_2")
                    .set_body_string("ok"),
            )
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ret":0})))
            .mount(&server)
            .await;

        let mut tf = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile()
            .expect("temp file");
        let plain = b"hello weixin send_file mock flow";
        tf.write_all(plain).expect("write plain");
        tf.flush().expect("flush");

        let adapter = WeChatAdapter::new(sample_cfg(&server.uri())).expect("adapter");
        adapter
            .send_ilink_file("wxid_target", tf.path(), None)
            .await
            .expect("send file");

        let requests = server.received_requests().await.expect("requests");

        let up_req = requests
            .iter()
            .find(|r| r.url.path() == "/ilink/bot/getuploadurl")
            .expect("getuploadurl request");
        let up_json: Value = serde_json::from_slice(&up_req.body).expect("upload json");
        assert_eq!(
            up_json.pointer("/to_user_id").and_then(|v| v.as_str()),
            Some("wxid_target")
        );
        assert_eq!(
            up_json.pointer("/media_type").and_then(|v| v.as_i64()),
            Some(MEDIA_FILE as i64)
        );
        assert_eq!(
            up_json.pointer("/rawsize").and_then(|v| v.as_u64()),
            Some(plain.len() as u64)
        );
        assert_eq!(
            up_json.pointer("/filesize").and_then(|v| v.as_u64()),
            Some(aes_padded_size(plain.len()) as u64)
        );
        let expected_md5 = WeChatAdapter::md5_hex(plain);
        assert_eq!(
            up_json.pointer("/rawfilemd5").and_then(|v| v.as_str()),
            Some(expected_md5.as_str())
        );
        let aes_hex = up_json
            .pointer("/aeskey")
            .and_then(|v| v.as_str())
            .expect("aeskey");
        assert_eq!(aes_hex.len(), 32);

        let cdn_req = requests
            .iter()
            .find(|r| r.url.path() == "/upload")
            .expect("cdn upload request");
        assert_eq!(
            cdn_req
                .url
                .query_pairs()
                .find(|(k, _)| k == "encrypted_query_param")
                .map(|(_, v)| v.to_string())
                .as_deref(),
            Some("up_param_1")
        );
        assert!(cdn_req
            .url
            .query_pairs()
            .any(|(k, v)| k == "filekey" && !v.is_empty()));
        assert_eq!(cdn_req.body.len() % 16, 0);
        assert_ne!(cdn_req.body, plain);

        let send_req = requests
            .iter()
            .find(|r| r.url.path() == "/ilink/bot/sendmessage")
            .expect("sendmessage request");
        let send_json: Value = serde_json::from_slice(&send_req.body).expect("send json");
        assert_eq!(
            send_json
                .pointer("/msg/to_user_id")
                .and_then(|v| v.as_str()),
            Some("wxid_target")
        );
        assert_eq!(
            send_json
                .pointer("/msg/item_list/0/type")
                .and_then(|v| v.as_i64()),
            Some(ITEM_FILE as i64)
        );
        assert_eq!(
            send_json
                .pointer("/msg/item_list/0/file_item/media/encrypt_query_param")
                .and_then(|v| v.as_str()),
            Some("enc_param_2")
        );
        let aes_b64 = send_json
            .pointer("/msg/item_list/0/file_item/media/aes_key")
            .and_then(|v| v.as_str())
            .expect("aes b64");
        let aes_raw = base64::engine::general_purpose::STANDARD
            .decode(aes_b64)
            .expect("decode aes key");
        assert_eq!(aes_raw.len(), 16);
    }
}

#[cfg(test)]
mod weixin_image_url_tests {
    use super::*;

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }
}
