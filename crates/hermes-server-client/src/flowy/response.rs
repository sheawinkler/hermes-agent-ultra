//! Flowy `{ code, msg, data }` API envelope parsing.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::ServerClientError;

/// Standard Flowy API response envelope.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FlowyEnvelope {
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub data: Value,
}

impl FlowyEnvelope {
    pub fn parse_body(text: &str) -> Result<Self, ServerClientError> {
        serde_json::from_str(text).map_err(|e| {
            ServerClientError::InvalidResponse(format!("not valid Flowy JSON envelope: {e}"))
        })
    }

    pub fn into_data<T: DeserializeOwned>(self) -> Result<T, ServerClientError> {
        if self.code == 401 {
            return Err(ServerClientError::AuthRequired(self.msg));
        }
        if self.code != 200 {
            return Err(ServerClientError::Api {
                code: self.code,
                msg: self.msg,
            });
        }
        if self.data.is_null() {
            return Err(ServerClientError::InvalidResponse(
                "success envelope missing data field".into(),
            ));
        }
        serde_json::from_value(self.data)
            .map_err(|e| ServerClientError::InvalidResponse(format!("data decode failed: {e}")))
    }

    pub fn ensure_ok_no_data(self) -> Result<(), ServerClientError> {
        if self.code == 401 {
            return Err(ServerClientError::AuthRequired(self.msg));
        }
        if self.code != 200 {
            return Err(ServerClientError::Api {
                code: self.code,
                msg: self.msg,
            });
        }
        Ok(())
    }

    pub fn into_data_opt<T: DeserializeOwned>(self) -> Result<Option<T>, ServerClientError> {
        if self.code == 401 {
            return Err(ServerClientError::AuthRequired(self.msg));
        }
        if self.code != 200 {
            return Err(ServerClientError::Api {
                code: self.code,
                msg: self.msg,
            });
        }
        if self.data.is_null() {
            return Ok(None);
        }
        serde_json::from_value(self.data)
            .map(Some)
            .map_err(|e| ServerClientError::InvalidResponse(format!("data decode failed: {e}")))
    }

    pub fn into_jwt_token(self) -> Result<String, ServerClientError> {
        if self.code == 401 {
            return Err(ServerClientError::AuthRequired(self.msg));
        }
        if self.code != 200 {
            return Err(ServerClientError::Api {
                code: self.code,
                msg: self.msg,
            });
        }
        match self.data {
            Value::String(token) if !token.is_empty() => Ok(token),
            _ => Err(ServerClientError::InvalidResponse(
                "login response data is not a JWT string".into(),
            )),
        }
    }
}

pub fn handle_http_and_envelope(
    http_status: u16,
    body: &str,
) -> Result<FlowyEnvelope, ServerClientError> {
    if http_status == 401 {
        if let Ok(env) = FlowyEnvelope::parse_body(body) {
            return Err(ServerClientError::AuthRequired(env.msg));
        }
        return Err(ServerClientError::AuthRequired(
            "HTTP 401 unauthorized".into(),
        ));
    }
    let env = FlowyEnvelope::parse_body(body)?;
    if http_status >= 400 && env.code == 200 {
        return Err(ServerClientError::from_http_status(
            http_status,
            body.to_string(),
            None,
        ));
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jwt_login_envelope() {
        let body = r#"{"code":200,"msg":"ok","data":"jwt-token-here"}"#;
        let env = FlowyEnvelope::parse_body(body).unwrap();
        assert_eq!(env.into_jwt_token().unwrap(), "jwt-token-here");
    }

    #[test]
    fn api_error_maps_code() {
        let body = r#"{"code":400,"msg":"验证码无效"}"#;
        let env = FlowyEnvelope::parse_body(body).unwrap();
        let err = env.into_data::<String>().unwrap_err();
        assert!(matches!(err, ServerClientError::Api { code: 400, .. }));
    }
}
