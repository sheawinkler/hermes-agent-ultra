//! E2E: cron 完成事件经 `webhooks.json` POST 到外部 HTTP（WireMock 收 webhook）。

use hermes_cli::webhook_delivery::{
    deliver_cron_completion_to_webhooks, save_webhook_store, webhook_http_client, WebhookRecord,
    WebhookStore,
};
use hermes_core::{AgentResult, Message};
use hermes_cron::{CronCompletionEvent, CronJob};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cron_completion_delivers_json_to_registered_webhook_url() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/notify"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let webhooks_path = dir.path().join("webhooks.json");
    let store = WebhookStore {
        webhooks: vec![WebhookRecord {
            id: "w1".to_string(),
            url: format!("{}/notify", mock.uri()),
            created_at: "2020-01-01T00:00:00Z".to_string(),
        }],
    };
    save_webhook_store(&webhooks_path, &store).expect("write webhooks.json");

    let job = CronJob::new("0 * * * *", "ping");
    let result = AgentResult {
        messages: vec![Message::assistant("done")],
        finished_naturally: true,
        total_turns: 1,
        tool_errors: vec![],
        usage: None,
    };
    let event = CronCompletionEvent::new(&job, "schedule", Ok(&result));

    let client = webhook_http_client().expect("http client");
    deliver_cron_completion_to_webhooks(&webhooks_path, &event, &client)
        .await
        .expect("delivery");

    mock.verify().await;

    let reqs = mock.received_requests().await.expect("requests");
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(reqs[0].body.as_slice()).expect("json body");
    assert_eq!(body["event"], "cron_job_finished");
    assert_eq!(body["job_id"], job.id);
    assert_eq!(body["trigger"], "schedule");
    assert_eq!(body["ok"], true);
    assert_eq!(body["assistant_snippet"], "done");
}
