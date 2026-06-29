use axum::Router;

use crate::HttpServerState;

pub mod apns;
pub mod cn;
pub mod fcm;
pub mod register;
pub mod router;
pub mod trigger;

pub fn routes() -> Router<HttpServerState> {
    Router::new()
        .merge(register::routes())
        .merge(router::routes())
}
