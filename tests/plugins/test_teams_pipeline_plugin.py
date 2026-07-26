"""Tests for the Teams pipeline plugin package."""

from __future__ import annotations

import asyncio
import json
import multiprocessing
import os
import stat
import threading
from types import SimpleNamespace
from pathlib import Path
from unittest.mock import MagicMock
import pytest

from hermes_cli.plugins import PluginContext, PluginManager, PluginManifest
from gateway.config import GatewayConfig, Platform, PlatformConfig
from plugins.teams_pipeline import register
from plugins.teams_pipeline.pipeline import TeamsMeetingPipeline
from plugins.teams_pipeline.store import (
    TeamsNotificationIdentityError,
    TeamsPipelineStore,
    TeamsPipelineStoreError,
)
from plugins.teams_pipeline.models import MeetingArtifact


class FakeGraphClient:
    def __init__(self) -> None:
        self.downloaded = False


async def _transcript_meeting_resolver(client, *, meeting_id=None, join_web_url=None, tenant_id=None):
    from plugins.teams_pipeline.models import TeamsMeetingRef

    return TeamsMeetingRef(
        meeting_id=str(meeting_id),
        tenant_id=tenant_id,
        metadata={"subject": "Weekly Sync", "participants": [{"displayName": "Ada"}]},
    )


async def _no_call_record(*args, **kwargs):
    return None


def _hold_effect_lock_in_child(store_path, sink_key, ready, release_event):
    store = TeamsPipelineStore(store_path)
    reservation = store.reserve_effect_started(
        sink_key,
        job_id="child-job",
        event_id="child-event",
        sink="teams",
    )
    ready.put(reservation.reserved)
    if reservation.reserved:
        release_event.wait(10)
        reservation.release()


def _leave_effect_started_in_child(store_path, sink_key, ready):
    store = TeamsPipelineStore(store_path)
    reservation = store.reserve_effect_started(
        sink_key,
        job_id="crashed-job",
        event_id="crashed-event",
        sink="teams",
    )
    ready.put(reservation.reserved)
    # Intentionally do not release: process exit must release the OS lock.


def test_register_adds_cli_only():
    mgr = PluginManager()
    manifest = PluginManifest(name="teams_pipeline")
    ctx = PluginContext(manifest, mgr)

    register(ctx)

    assert "teams-pipeline" in mgr._cli_commands
    entry = mgr._cli_commands["teams-pipeline"]
    assert entry["plugin"] == "teams_pipeline"
    assert callable(entry["setup_fn"])
    assert callable(entry["handler_fn"])


def test_runtime_config_uses_existing_teams_platform_settings():
    from plugins.teams_pipeline.runtime import build_pipeline_runtime_config

    gateway_config = GatewayConfig(
        platforms={
            Platform("teams"): PlatformConfig(
                enabled=True,
                extra={
                    "delivery_mode": "graph",
                    "team_id": "team-1",
                    "channel_id": "channel-1",
                    "meeting_pipeline": {
                        "transcript_min_chars": 120,
                        "notion": {"enabled": True, "database_id": "db-1"},
                    },
                },
            )
        }
    )

    runtime_config = build_pipeline_runtime_config(gateway_config)

    assert runtime_config["transcript_min_chars"] == 120
    assert runtime_config["notion"]["database_id"] == "db-1"
    assert runtime_config["teams_delivery"] == {
        "enabled": True,
        "mode": "graph",
        "team_id": "team-1",
        "channel_id": "channel-1",
    }


def test_build_pipeline_runtime_reuses_existing_teams_adapter_surface(monkeypatch, tmp_path):
    from plugins.teams_pipeline import runtime as runtime_module

    class FakeWriter:
        def __init__(self, platform_config=None, **kwargs) -> None:
            self.platform_config = platform_config

    monkeypatch.setattr(runtime_module, "build_graph_client", lambda: object())
    monkeypatch.setattr(runtime_module, "resolve_teams_pipeline_store_path", lambda: tmp_path / "teams-store.json")
    monkeypatch.setattr("plugins.platforms.teams.adapter.TeamsSummaryWriter", FakeWriter)

    gateway = SimpleNamespace(
        config=GatewayConfig(
            platforms={
                Platform("teams"): PlatformConfig(
                    enabled=True,
                    extra={
                        "delivery_mode": "incoming_webhook",
                        "incoming_webhook_url": "https://example.com/hook",
                    },
                )
            }
        )
    )

    runtime = runtime_module.build_pipeline_runtime(gateway)

    assert isinstance(runtime.teams_sender, FakeWriter)
    assert runtime.teams_sender.platform_config is gateway.config.platforms[Platform("teams")]



def test_enabled_teams_delivery_rejects_missing_writer(monkeypatch):
    from plugins.teams_pipeline import runtime as runtime_module

    def missing_writer():
        raise ImportError("Teams adapter unavailable")

    monkeypatch.setattr(runtime_module, "_load_teams_summary_writer", missing_writer)
    gateway = SimpleNamespace(
        config=GatewayConfig(
            platforms={
                Platform("teams"): PlatformConfig(
                    enabled=True,
                    extra={
                        "delivery_mode": "incoming_webhook",
                        "incoming_webhook_url": "https://example.com/hook",
                    },
                )
            }
        )
    )

    with pytest.raises(runtime_module.TeamsPipelineRuntimeError, match="enabled"):
        runtime_module.build_pipeline_runtime(gateway)


def test_enabled_teams_delivery_rejects_disabled_teams_platform():
    from plugins.teams_pipeline import runtime as runtime_module

    gateway = SimpleNamespace(
        config=GatewayConfig(
            platforms={
                Platform("teams"): PlatformConfig(
                    enabled=False,
                    extra={
                        "meeting_pipeline": {
                            "teams_delivery": {
                                "enabled": True,
                                "mode": "incoming_webhook",
                                "incoming_webhook_url": "https://example.com/hook",
                            }
                        }
                    },
                )
            }
        )
    )

    with pytest.raises(runtime_module.TeamsPipelineRuntimeError, match="platform"):
        runtime_module.build_pipeline_runtime(gateway)


@pytest.mark.anyio
async def test_build_runtime_attaches_and_invokes_enabled_notion_and_linear_writers(
    monkeypatch, tmp_path
):
    from plugins.teams_pipeline import pipeline as pipeline_module
    from plugins.teams_pipeline import runtime as runtime_module

    notion_calls = []
    linear_calls = []

    class FakeNotionWriter:
        async def write_summary(self, payload, config, existing_record=None):
            notion_calls.append((payload, config, existing_record))
            return {"page_id": "page-runtime"}

    class FakeLinearWriter:
        async def write_summary(self, payload, config, existing_record=None):
            linear_calls.append((payload, config, existing_record))
            return {"issue_id": "issue-runtime"}

    monkeypatch.setattr(runtime_module, "build_graph_client", lambda: object())
    monkeypatch.setattr(
        runtime_module,
        "resolve_teams_pipeline_store_path",
        lambda: tmp_path / "teams-store.json",
    )
    monkeypatch.setattr(runtime_module, "NotionWriter", FakeNotionWriter)
    monkeypatch.setattr(runtime_module, "LinearWriter", FakeLinearWriter)
    gateway = SimpleNamespace(
        config=GatewayConfig(
            platforms={
                Platform("teams"): PlatformConfig(
                    enabled=True,
                    extra={
                        "meeting_pipeline": {
                            "notion": {"enabled": True, "database_id": "db-runtime"},
                            "linear": {"enabled": True, "team_id": "team-runtime"},
                        }
                    },
                )
            }
        )
    )

    runtime = runtime_module.build_pipeline_runtime(gateway)
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-runtime",
        event_id="event-runtime",
        source_event_type="updated",
        dedupe_key="event-runtime",
        status="summarizing",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-runtime",
            tenant_id="tenant-runtime",
        ),
    )
    payload = pipeline_module.TeamsMeetingSummaryPayload(
        meeting_ref=job.meeting_ref,
        summary="Runtime writer summary",
    )

    await runtime._write_sinks(job, payload)

    assert isinstance(runtime.notion_writer, FakeNotionWriter)
    assert isinstance(runtime.linear_writer, FakeLinearWriter)
    assert notion_calls[0][1]["database_id"] == "db-runtime"
    assert linear_calls[0][1]["team_id"] == "team-runtime"

@pytest.mark.anyio
async def test_bind_gateway_runtime_attaches_scheduler(monkeypatch, tmp_path):
    from plugins.teams_pipeline import runtime as runtime_module

    class FakeAdapter:
        def __init__(self) -> None:
            self.scheduler = None

        def set_notification_scheduler(self, scheduler) -> None:
            self.scheduler = scheduler

    class FakePipeline:
        def __init__(self) -> None:
            self.notifications = []

        async def run_notification(self, notification):
            self.notifications.append(notification)

    adapter = FakeAdapter()
    pipeline = FakePipeline()
    gateway = SimpleNamespace(
        adapters={Platform.MSGRAPH_WEBHOOK: adapter},
        config=GatewayConfig(platforms={}),
        _teams_pipeline_runtime=None,
        _teams_pipeline_runtime_error=None,
    )

    monkeypatch.setattr(runtime_module, "build_pipeline_runtime", lambda gateway_runner: pipeline)

    bound = runtime_module.bind_gateway_runtime(gateway)

    assert bound is True
    assert gateway._teams_pipeline_runtime is pipeline
    assert callable(adapter.scheduler)

    notification = {"id": "notif-1"}
    await adapter.scheduler(notification, object())
    assert pipeline.notifications == [notification]


def test_bind_gateway_runtime_propagates_failure_without_installing_drop_scheduler(monkeypatch):
    from plugins.teams_pipeline import runtime as runtime_module
    from tools.microsoft_graph_auth import MicrosoftGraphConfigError

    class FakeAdapter:
        def __init__(self) -> None:
            self.scheduler = None
            self.scheduler_calls = 0

        def set_notification_scheduler(self, scheduler) -> None:
            self.scheduler_calls += 1
            self.scheduler = scheduler

    adapter = FakeAdapter()
    gateway = SimpleNamespace(
        adapters={Platform.MSGRAPH_WEBHOOK: adapter},
        config=GatewayConfig(platforms={}),
        _teams_pipeline_runtime=None,
        _teams_pipeline_runtime_error=None,
    )

    def _raise(_gateway_runner):
        raise MicrosoftGraphConfigError("missing graph env")

    monkeypatch.setattr(runtime_module, "build_pipeline_runtime", _raise)

    with pytest.raises(MicrosoftGraphConfigError, match="missing graph env"):
        runtime_module.bind_gateway_runtime(gateway)

    assert gateway._teams_pipeline_runtime is None
    assert "missing graph env" in gateway._teams_pipeline_runtime_error
    assert adapter.scheduler is None
    assert adapter.scheduler_calls == 0


def test_store_persists_subscription_event_and_job_state(tmp_path):
    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    store.upsert_subscription(
        "sub-1",
        {"client_state": "abc", "resource": "communications/onlineMeetings"},
    )
    store.record_event_timestamp("evt-1", "2026-05-03T19:30:00Z")
    store.upsert_job("job-1", {"status": "received", "event_id": "evt-1"})
    store.upsert_sink_record("notion:meeting-1", {"page_id": "page-1"})

    reloaded = TeamsPipelineStore(store_path)
    subscription = reloaded.get_subscription("sub-1")
    job = reloaded.get_job("job-1")
    sink = reloaded.get_sink_record("notion:meeting-1")

    assert subscription is not None
    assert subscription["subscription_id"] == "sub-1"
    assert "client_state" not in subscription
    assert reloaded.get_event_timestamp("evt-1") == "2026-05-03T19:30:00Z"
    assert job is not None
    assert job["status"] == "received"
    assert sink is not None
    assert sink["page_id"] == "page-1"


def test_store_notification_receipts_are_idempotent(tmp_path):
    store = TeamsPipelineStore(tmp_path / "teams-store.json")
    notification = {
        "subscriptionId": "sub-1",
        "resource": "communications/onlineMeetings/meeting-1",
        "changeType": "updated",
    }
    receipt_key = TeamsPipelineStore.build_notification_receipt_key(notification)

    assert store.record_notification_receipt(receipt_key, notification) is True
    assert store.record_notification_receipt(receipt_key, notification) is False
    assert store.has_notification_receipt(receipt_key) is True

    reloaded = TeamsPipelineStore(tmp_path / "teams-store.json")
    assert reloaded.has_notification_receipt(receipt_key) is True


@pytest.mark.anyio
class TestTeamsMeetingPipeline:
    async def test_transcript_first_path_persists_state_and_skips_recording(self, tmp_path, monkeypatch):
        from plugins.teams_pipeline import pipeline as pipeline_module

        monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)

        async def _fetch_transcript(client, meeting_ref):
            return (
                MeetingArtifact(artifact_type="transcript", artifact_id="tx-1", display_name="meeting.vtt"),
                "Action: Send draft by Friday.\nDecision: Ship the transcript-first path.\nRAW_TRANSCRIPT_SENTINEL",
            )

        async def _call_record(client, meeting_ref, *, call_record_id=None, allow_permission_errors=True):
            return MeetingArtifact(
                artifact_type="call_record",
                artifact_id="call-1",
                metadata={"metrics": {"participant_count": 4}},
            )

        async def _summarize(**kwargs):
            return pipeline_module.TeamsMeetingSummaryPayload(
                meeting_ref=kwargs["resolved_meeting"],
                title="Weekly Sync",
                transcript_text=kwargs["transcript_text"],
                summary="Short summary",
                key_decisions=["Ship the transcript-first path."],
                action_items=["Send draft by Friday."],
                risks=["Timeline risk."],
                confidence="high",
                confidence_notes="Transcript available.",
                source_artifacts=kwargs["artifacts"],
            )

        monkeypatch.setattr(pipeline_module, "fetch_preferred_transcript_text", _fetch_transcript)
        monkeypatch.setattr(pipeline_module, "enrich_meeting_with_call_record", _call_record)

        store = TeamsPipelineStore(tmp_path / "teams-store.json")
        pipeline = TeamsMeetingPipeline(
            graph_client=FakeGraphClient(),
            store=store,
            config={"transcript_min_chars": 20},
            summarize_fn=_summarize,
        )

        job = await pipeline.run_notification(
            {
                "id": "notif-1",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-123",
                "resourceData": {"id": "meeting-123"},
                "clientState": "CLIENT_STATE_SENTINEL",
            }
        )

        assert job.status == "completed"
        assert job.selected_artifact_strategy == "transcript_first"
        assert job.summary_payload is not None
        assert job.summary_payload.summary == "Short summary"
        stored = store.get_job(job.job_id)
        assert stored is not None
        assert stored["status"] == "completed"
        assert stored["summary_payload"]["summary"] == "Short summary"
        assert "transcript_text" not in stored["summary_payload"]
        persisted_text = (tmp_path / "teams-store.json").read_text(encoding="utf-8")
        assert "RAW_TRANSCRIPT_SENTINEL" not in persisted_text
        assert "CLIENT_STATE_SENTINEL" not in persisted_text

    async def test_recording_fallback_uses_stt_and_updates_sink_records(self, tmp_path, monkeypatch):
        from plugins.teams_pipeline import pipeline as pipeline_module

        monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)

        async def _no_transcript(client, meeting_ref):
            return None, None

        async def _recordings(client, meeting_ref):
            return [
                MeetingArtifact(
                    artifact_type="recording",
                    artifact_id="rec-1",
                    display_name="recording.mp4",
                    download_url="https://files.example/recording.mp4",
                )
            ]

        async def _download(client, meeting_ref, recording, destination):
            target = Path(destination)
            target.write_bytes(b"video-bytes")
            return {"path": str(target), "size_bytes": 11, "content_type": "video/mp4"}

        async def _prepare_audio(self, recording_path):
            audio_path = recording_path.with_suffix(".wav")
            audio_path.write_bytes(b"audio-bytes")
            return audio_path

        def _transcribe(file_path, model):
            return {"success": True, "transcript": "Action: Follow up with Legal.\nRisk: Budget approval pending.", "provider": "local"}

        async def _summarize(**kwargs):
            return pipeline_module.TeamsMeetingSummaryPayload(
                meeting_ref=kwargs["resolved_meeting"],
                title="Weekly Sync",
                transcript_text=kwargs["transcript_text"],
                summary="Fallback summary",
                key_decisions=[],
                action_items=["Follow up with Legal."],
                risks=["Budget approval pending."],
                confidence="medium",
                confidence_notes="Generated from STT fallback.",
                source_artifacts=kwargs["artifacts"],
            )

        class FakeNotionWriter:
            async def write_summary(self, payload, config, existing_record=None):
                return {"page_id": existing_record.get("page_id") if existing_record else "page-1", "url": "https://notion.so/page-1"}

        async def _teams_sender(payload, config, existing_record=None):
            return {"message_id": existing_record.get("message_id") if existing_record else "msg-1"}

        monkeypatch.setattr(pipeline_module, "fetch_preferred_transcript_text", _no_transcript)
        monkeypatch.setattr(pipeline_module, "list_recording_artifacts", _recordings)
        monkeypatch.setattr(pipeline_module, "download_recording_artifact", _download)
        monkeypatch.setattr(pipeline_module.TeamsMeetingPipeline, "_prepare_audio_path", _prepare_audio)
        monkeypatch.setattr(pipeline_module, "enrich_meeting_with_call_record", _no_call_record)

        store = TeamsPipelineStore(tmp_path / "teams-store.json")
        pipeline = TeamsMeetingPipeline(
            graph_client=FakeGraphClient(),
            store=store,
            config={
                "notion": {"enabled": True, "database_id": "db-1"},
                "teams_delivery": {"enabled": True, "channel_id": "channel-1"},
            },
            transcribe_fn=_transcribe,
            summarize_fn=_summarize,
            notion_writer=FakeNotionWriter(),
            teams_sender=_teams_sender,
        )

        job = await pipeline.run_notification(
            {
                "id": "notif-2",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-456",
                "resourceData": {"id": "meeting-456"},
            }
        )

        assert job.status == "completed"
        assert job.selected_artifact_strategy == "recording_stt_fallback"
        assert job.summary_payload is not None
        assert job.summary_payload.summary == "Fallback summary"
        notion_record = store.get_sink_record(pipeline._build_sink_effect_key("notion", job))
        teams_record = store.get_sink_record(pipeline._build_sink_effect_key("teams", job))
        assert notion_record is not None
        assert notion_record["page_id"] == "page-1"
        assert teams_record is not None
        assert teams_record["message_id"] == "msg-1"

    async def test_missing_transcript_and_recording_schedules_retry(self, tmp_path, monkeypatch):
        from plugins.teams_pipeline import pipeline as pipeline_module

        monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)
        monkeypatch.setattr(pipeline_module, "fetch_preferred_transcript_text", lambda *a, **kw: asyncio.sleep(0, result=(None, None)))
        monkeypatch.setattr(pipeline_module, "list_recording_artifacts", lambda *a, **kw: asyncio.sleep(0, result=[]))

        store = TeamsPipelineStore(tmp_path / "teams-store.json")
        pipeline = TeamsMeetingPipeline(
            graph_client=FakeGraphClient(),
            store=store,
            config={},
            summarize_fn=lambda **kwargs: asyncio.sleep(0, result=None),
        )

        job = await pipeline.run_notification(
            {
                "id": "notif-3",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-789",
                "resourceData": {"id": "meeting-789"},
            }
        )

        assert job.status == "retry_scheduled"
        assert job.error_info["retryable"] is True
        assert "Recording unavailable" in job.error_info["message"]

    async def test_duplicate_notification_reuses_completed_job(self, tmp_path, monkeypatch):
        from plugins.teams_pipeline import pipeline as pipeline_module

        monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)

        async def _fetch_transcript(client, meeting_ref):
            return (
                MeetingArtifact(artifact_type="transcript", artifact_id="tx-dup", display_name="meeting.vtt"),
                "Decision: Keep duplicate notifications idempotent.\nAction: Verify the cached job is reused.",
            )

        summarize_calls = 0

        async def _summarize(**kwargs):
            nonlocal summarize_calls
            summarize_calls += 1
            return pipeline_module.TeamsMeetingSummaryPayload(
                meeting_ref=kwargs["resolved_meeting"],
                title="Weekly Sync",
                transcript_text=kwargs["transcript_text"],
                summary="Duplicate-safe summary",
                key_decisions=["Keep duplicate notifications idempotent."],
                action_items=["Verify the cached job is reused."],
                confidence="high",
                confidence_notes="Transcript available.",
                source_artifacts=kwargs["artifacts"],
            )

        monkeypatch.setattr(pipeline_module, "fetch_preferred_transcript_text", _fetch_transcript)
        monkeypatch.setattr(pipeline_module, "enrich_meeting_with_call_record", _no_call_record)

        store = TeamsPipelineStore(tmp_path / "teams-store.json")
        pipeline = TeamsMeetingPipeline(
            graph_client=FakeGraphClient(),
            store=store,
            config={"transcript_min_chars": 20},
            summarize_fn=_summarize,
        )
        notification = {
            "id": "notif-dup",
            "changeType": "updated",
            "resource": "communications/onlineMeetings/meeting-dup",
            "resourceData": {"id": "meeting-dup"},
        }

        first_job = await pipeline.run_notification(notification)
        second_job = await pipeline.run_notification(notification)

        assert first_job.status == "completed"
        assert second_job.status == "completed"
        assert second_job.job_id == first_job.job_id
        assert summarize_calls == 1
        assert len(store.list_jobs()) == 1
        receipt_key = TeamsPipelineStore.build_notification_receipt_key(notification)
        assert store.has_notification_receipt(receipt_key) is True


def test_notification_receipt_identity_ignores_volatile_envelope_fields():
    slash_form = {
        "tenantId": "TENANT-A",
        "subscriptionId": "SUB-1",
        "changeType": "created",
        "resource": "communications/onlineMeetings/meeting-1/transcripts/transcript-1",
        "resourceData": {
            "id": "transcript-1",
            "@odata.id": "communications/onlineMeetings/meeting-1/transcripts/transcript-1",
        },
    }
    volatile_variant = {
        **slash_form,
        "id": "delivery-2",
        "expirationDateTime": "2030-01-01T00:00:00Z",
        "clientState": "CLIENT_STATE_SENTINEL",
        "resourceData": {
            **slash_form["resourceData"],
            "encryptionCertificateId": "rotated",
            "validationTokens": ["volatile"],
        },
    }
    parenthesized_form = {
        **slash_form,
        "resource": "communications/onlineMeetings('meeting-1')/transcripts('transcript-1')",
        "resourceData": {
            "id": "transcript-1",
            "@odata.id": "communications/onlineMeetings('meeting-1')/transcripts('transcript-1')",
        },
    }
    distinct_artifact = {
        **slash_form,
        "resource": "communications/onlineMeetings/meeting-1/transcripts/transcript-2",
        "resourceData": {
            "id": "transcript-2",
            "@odata.id": "communications/onlineMeetings/meeting-1/transcripts/transcript-2",
        },
    }

    key = TeamsPipelineStore.build_notification_receipt_key(slash_form)
    assert key == TeamsPipelineStore.build_notification_receipt_key(volatile_variant)
    assert key == TeamsPipelineStore.build_notification_receipt_key(parenthesized_form)
    assert key != TeamsPipelineStore.build_notification_receipt_key(distinct_artifact)
    assert not key.startswith("sha256:")

    with pytest.raises(TeamsNotificationIdentityError):
        TeamsPipelineStore.build_notification_receipt_key(
            {
                **slash_form,
                "resourceData": {
                    "id": "different-transcript",
                    "@odata.id": slash_form["resourceData"]["@odata.id"],
                },
            }
        )
    with pytest.raises(TeamsNotificationIdentityError):
        TeamsPipelineStore.build_notification_receipt_key(
            {
                "subscriptionId": "sub-1",
                "changeType": "created",
                "resource": "communications/onlineMeetings/getAllTranscripts",
            }
        )


def test_store_rejects_malformed_and_symlink_destinations(tmp_path):
    malformed_path = tmp_path / "malformed.json"
    malformed_path.write_text("{not-json", encoding="utf-8")
    malformed_path.chmod(0o600)
    with pytest.raises(TeamsPipelineStoreError, match="malformed JSON"):
        TeamsPipelineStore(malformed_path)

    target_path = tmp_path / "target.json"
    target_path.write_text("{}", encoding="utf-8")
    target_path.chmod(0o600)
    symlink_path = tmp_path / "symlink.json"
    symlink_path.symlink_to(target_path)
    with pytest.raises(TeamsPipelineStoreError, match="regular file"):
        TeamsPipelineStore(symlink_path)


def test_python_store_marker_rejects_rust_owned_store(tmp_path):
    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    store.upsert_job("job-1", {"status": "received", "event_id": "event-1"})

    document = json.loads(store_path.read_text(encoding="utf-8"))
    assert document["store_format"] == "hermes.teams_pipeline.python.v1"
    assert document["runtime"] == "python"

    rust_store_path = tmp_path / "rust-teams-store.json"
    rust_store_path.write_text(
        json.dumps(
            {
                "store_format": "hermes.teams_pipeline.rust.v1",
                "runtime": "rust",
            }
        ),
        encoding="utf-8",
    )
    rust_store_path.chmod(0o600)
    with pytest.raises(TeamsPipelineStoreError, match="Rust-marked"):
        TeamsPipelineStore(rust_store_path)


def test_store_write_failure_preserves_memory_and_disk(tmp_path, monkeypatch):
    from plugins.teams_pipeline import store as store_module

    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    store.upsert_subscription("sub-1", {"resource": "communications/onlineMeetings"})
    disk_before = store_path.read_bytes()

    def fail_replace(_source, _destination):
        raise OSError("injected replace failure")

    monkeypatch.setattr(store_module.os, "replace", fail_replace)
    with pytest.raises(TeamsPipelineStoreError, match="persist"):
        store.upsert_subscription("sub-2", {"resource": "communications/onlineMeetings"})

    assert store_path.read_bytes() == disk_before
    assert store.get_subscription("sub-1") is not None
    assert store.get_subscription("sub-2") is None


def test_store_serializes_concurrent_copy_on_write_updates(tmp_path):
    store_path = tmp_path / "teams-store.json"
    stores = [TeamsPipelineStore(store_path) for _ in range(8)]
    start = threading.Barrier(len(stores))
    failures: list[BaseException] = []

    def write_job(index: int, store: TeamsPipelineStore) -> None:
        try:
            start.wait()
            store.upsert_job(
                f"concurrent-{index}",
                {"status": "received", "event_id": f"event-{index}", "dedupe_key": f"event-{index}"},
            )
        except BaseException as exc:  # pragma: no cover - asserted below
            failures.append(exc)

    threads = [
        threading.Thread(target=write_job, args=(index, store), daemon=True)
        for index, store in enumerate(stores)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert failures == []
    reloaded = TeamsPipelineStore(store_path)
    assert set(reloaded.list_jobs()) == {f"concurrent-{index}" for index in range(len(stores))}


@pytest.mark.skipif(os.name == "nt", reason="POSIX permissions are not available on Windows")
def test_store_writes_owner_only_state_file(tmp_path):
    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    store.upsert_job("job-1", {"status": "received", "event_id": "event-1"})

    assert stat.S_IMODE(store_path.stat().st_mode) == 0o600


def test_store_bounds_terminal_payloads_without_evicting_active_or_retry_jobs(tmp_path):
    store = TeamsPipelineStore(
        tmp_path / "count-bound.json",
        job_retention_max_count=2,
        job_retention_max_bytes=1024 * 1024,
    )
    store.upsert_job("active", {"status": "received", "event_id": "active"})
    store.upsert_job("old-terminal", {"status": "completed", "event_id": "old"})
    store.upsert_job("new-terminal", {"status": "completed", "event_id": "new"})

    assert store.get_job("active") is not None
    assert store.get_job("new-terminal") is not None
    assert store.get_job("old-terminal") is None

    byte_bound_store = TeamsPipelineStore(
        tmp_path / "byte-bound.json",
        job_retention_max_count=10,
        job_retention_max_bytes=1,
        job_retention_ttl_seconds=0,
    )
    byte_bound_store.upsert_job("retry", {"status": "retry_scheduled", "event_id": "retry"})
    byte_bound_store.upsert_job(
        "terminal",
        {"status": "completed", "event_id": "terminal", "summary_payload": {"summary": "x" * 4096}},
    )

    assert byte_bound_store.get_job("retry") is not None
    assert byte_bound_store.get_job("terminal") is None


@pytest.mark.anyio
async def test_pruned_job_duplicate_reuses_tombstone_without_repeating_sink(tmp_path, monkeypatch):
    from plugins.teams_pipeline import pipeline as pipeline_module

    monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)
    monkeypatch.setattr(
        pipeline_module,
        "fetch_preferred_transcript_text",
        lambda *args, **kwargs: asyncio.sleep(
            0,
            result=(
                MeetingArtifact(artifact_type="transcript", artifact_id="transcript-pruned"),
                "RAW_TRANSCRIPT_SENTINEL",
            ),
        ),
    )
    monkeypatch.setattr(pipeline_module, "enrich_meeting_with_call_record", _no_call_record)
    summary_calls = 0
    sink_calls = 0

    async def summarize(**kwargs):
        nonlocal summary_calls
        summary_calls += 1
        return pipeline_module.TeamsMeetingSummaryPayload(
            meeting_ref=kwargs["resolved_meeting"],
            summary="Bounded summary",
            transcript_text=kwargs["transcript_text"],
        )

    async def send_summary(payload, config, existing_record=None):
        nonlocal sink_calls
        sink_calls += 1
        return {"message_id": "message-pruned"}

    store = TeamsPipelineStore(
        tmp_path / "teams-store.json",
        job_retention_max_count=0,
        job_retention_max_bytes=0,
    )
    pipeline = TeamsMeetingPipeline(
        graph_client=FakeGraphClient(),
        store=store,
        config={"transcript_min_chars": 1, "teams_delivery": {"enabled": True, "channel_id": "channel-1"}},
        summarize_fn=summarize,
        teams_sender=send_summary,
    )
    first_notification = {
        "id": "volatile-delivery-1",
        "tenantId": "tenant-a",
        "subscriptionId": "sub-a",
        "changeType": "updated",
        "resource": "communications/onlineMeetings/meeting-pruned",
        "resourceData": {"id": "meeting-pruned"},
        "clientState": "CLIENT_STATE_SENTINEL",
    }

    first = await pipeline.run_notification(first_notification)
    second = await pipeline.run_notification(
        {
            **first_notification,
            "id": "volatile-delivery-2",
            "expirationDateTime": "2030-01-01T00:00:00Z",
            "clientState": "ROTATED_CLIENT_STATE_SENTINEL",
        }
    )

    assert first.status == "completed"
    assert store.get_job(first.job_id) is None
    assert second.status == "completed"
    assert second.job_id == first.job_id
    assert summary_calls == 1
    assert sink_calls == 1
    persisted_text = (tmp_path / "teams-store.json").read_text(encoding="utf-8")
    assert "RAW_TRANSCRIPT_SENTINEL" not in persisted_text
    assert "CLIENT_STATE_SENTINEL" not in persisted_text
    assert "ROTATED_CLIENT_STATE_SENTINEL" not in persisted_text


@pytest.mark.anyio
async def test_response_lost_after_sink_commit_blocks_duplicate_create(tmp_path, monkeypatch):
    from plugins.teams_pipeline import pipeline as pipeline_module

    monkeypatch.setattr(pipeline_module, "resolve_meeting_reference", _transcript_meeting_resolver)
    monkeypatch.setattr(
        pipeline_module,
        "fetch_preferred_transcript_text",
        lambda *args, **kwargs: asyncio.sleep(
            0,
            result=(
                MeetingArtifact(artifact_type="transcript", artifact_id="transcript-sink"),
                "Transcript for sink safety.",
            ),
        ),
    )
    monkeypatch.setattr(pipeline_module, "enrich_meeting_with_call_record", _no_call_record)
    sink_creates = 0

    async def summarize(**kwargs):
        return pipeline_module.TeamsMeetingSummaryPayload(
            meeting_ref=kwargs["resolved_meeting"],
            summary="Sink-safe summary",
            transcript_text=kwargs["transcript_text"],
        )

    async def response_lost_sender(payload, config, existing_record=None):
        nonlocal sink_creates
        sink_creates += 1
        raise TimeoutError("remote effect committed but response was lost")

    store = TeamsPipelineStore(tmp_path / "teams-store.json")
    pipeline = TeamsMeetingPipeline(
        graph_client=FakeGraphClient(),
        store=store,
        config={"transcript_min_chars": 1, "teams_delivery": {"enabled": True, "channel_id": "channel-1"}},
        summarize_fn=summarize,
        teams_sender=response_lost_sender,
    )
    notification = {
        "tenantId": "tenant-a",
        "subscriptionId": "sub-a",
        "changeType": "updated",
        "resource": "communications/onlineMeetings/meeting-sink",
        "resourceData": {"id": "meeting-sink"},
    }

    first = await pipeline.run_notification(notification)
    second = await pipeline.run_job(first.job_id)

    sink_record = store.get_sink_record(pipeline._build_sink_effect_key("teams", first))
    assert first.status == "failed"
    assert second.status == "failed"
    assert sink_creates == 1
    assert sink_record is not None
    assert sink_record["effect_status"] == "outcome_unknown"


def test_store_fails_closed_when_cross_process_locking_is_unavailable(tmp_path, monkeypatch):
    from plugins.teams_pipeline import store as store_module

    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    monkeypatch.setattr(store_module, "fcntl", None)
    monkeypatch.setattr(store_module, "msvcrt", None)

    with pytest.raises(TeamsPipelineStoreError, match="Cross-process"):
        store.upsert_job("job-1", {"status": "received", "event_id": "event-1"})
    assert not store_path.exists()


@pytest.mark.anyio
async def test_sink_effect_reservation_is_atomic_across_store_instances(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    store_path = tmp_path / "teams-store.json"
    first_pipeline = TeamsMeetingPipeline(
        graph_client=FakeGraphClient(),
        store=TeamsPipelineStore(store_path),
    )
    second_pipeline = TeamsMeetingPipeline(
        graph_client=FakeGraphClient(),
        store=TeamsPipelineStore(store_path),
    )
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-atomic",
        event_id="event-atomic",
        source_event_type="updated",
        dedupe_key="event-atomic",
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(meeting_id="meeting-atomic"),
    )
    create_calls = 0

    async def create_once(existing_record=None):
        nonlocal create_calls
        create_calls += 1
        await asyncio.sleep(0.02)
        return {"message_id": "message-atomic"}

    results = await asyncio.gather(
        first_pipeline._write_sink_once(job, "teams", create_once),
        second_pipeline._write_sink_once(job, "teams", create_once),
        return_exceptions=True,
    )

    assert create_calls == 1
    assert sum(result is None for result in results) == 1
    assert sum(
        isinstance(result, pipeline_module.TeamsPipelineSinkError) for result in results
    ) == 1


@pytest.mark.anyio
async def test_conclusive_no_effect_sink_error_allows_a_later_retry(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    store = TeamsPipelineStore(tmp_path / "teams-store.json")
    pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=store)
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-retry",
        event_id="event-retry",
        source_event_type="updated",
        dedupe_key="event-retry",
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(meeting_id="meeting-retry"),
    )
    attempts = 0

    async def writer(existing_record=None):
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise pipeline_module.TeamsPipelineSinkNoEffectError("local validation failed")
        return {"message_id": "message-retry"}

    with pytest.raises(pipeline_module.TeamsPipelineSinkNoEffectError):
        await pipeline._write_sink_once(job, "teams", writer)
    await pipeline._write_sink_once(job, "teams", writer)

    assert attempts == 2
    assert store.get_sink_record(pipeline._build_sink_effect_key("teams", job))["effect_status"] == "completed"


@pytest.mark.anyio
async def test_live_effect_lock_blocks_second_process_without_remote_call(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    if "fork" not in multiprocessing.get_all_start_methods():
        pytest.skip("This process-overlap contract requires a fork-capable platform.")
    context = multiprocessing.get_context("fork")
    store_path = tmp_path / "teams-store.json"
    store = TeamsPipelineStore(store_path)
    pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=store)
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-overlap",
        event_id="event-overlap",
        source_event_type="updated",
        dedupe_key="event-overlap",
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-overlap",
            tenant_id="tenant-overlap",
        ),
    )
    sink_key = pipeline._build_sink_effect_key("teams", job)
    ready = context.Queue()
    release_event = context.Event()
    process = context.Process(
        target=_hold_effect_lock_in_child,
        args=(str(store_path), sink_key, ready, release_event),
    )
    process.start()
    try:
        assert ready.get(timeout=5) is True
        remote_calls = 0

        async def remote_writer(existing_record=None):
            nonlocal remote_calls
            remote_calls += 1
            return {"message_id": "must-not-exist"}

        with pytest.raises(pipeline_module.TeamsPipelineSinkError, match="actively locked"):
            await pipeline._write_sink_once(job, "teams", remote_writer)

        assert remote_calls == 0
        live_record = TeamsPipelineStore(store_path).get_sink_record(sink_key)
        assert live_record is not None
        assert live_record["effect_status"] == "effect_started"
    finally:
        release_event.set()
        process.join(timeout=5)
        if process.is_alive():
            process.terminate()
            process.join(timeout=5)

    assert process.exitcode == 0


@pytest.mark.anyio
async def test_process_death_recovers_effect_started_as_unknown_without_remote_retry(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    if "fork" not in multiprocessing.get_all_start_methods():
        pytest.skip("This process-death contract requires a fork-capable platform.")
    context = multiprocessing.get_context("fork")
    store_path = tmp_path / "teams-store.json"
    initial_store = TeamsPipelineStore(store_path)
    initial_pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=initial_store)
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-crash",
        event_id="event-crash",
        source_event_type="updated",
        dedupe_key="event-crash",
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-crash",
            tenant_id="tenant-crash",
        ),
    )
    sink_key = initial_pipeline._build_sink_effect_key("teams", job)
    ready = context.Queue()
    process = context.Process(
        target=_leave_effect_started_in_child,
        args=(str(store_path), sink_key, ready),
    )
    process.start()
    assert ready.get(timeout=5) is True
    process.join(timeout=5)
    if process.is_alive():
        process.terminate()
        process.join(timeout=5)
    assert process.exitcode == 0

    restarted_store = TeamsPipelineStore(store_path)
    restarted_pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=restarted_store)
    remote_calls = 0

    async def remote_writer(existing_record=None):
        nonlocal remote_calls
        remote_calls += 1
        return {"message_id": "must-not-exist"}

    with pytest.raises(pipeline_module.TeamsPipelineSinkError, match="outcome is unknown"):
        await restarted_pipeline._write_sink_once(job, "teams", remote_writer)

    record = restarted_store.get_sink_record(sink_key)
    assert remote_calls == 0
    assert record is not None
    assert record["effect_status"] == "outcome_unknown"
    assert record["effect_recovery_reason"] == "effect_lock_released_before_outcome"
    assert restarted_store.list_outcome_unknown_sink_records() == {sink_key: record}


@pytest.mark.anyio
async def test_sink_effect_scope_ignores_subscription_renewal_and_change_type(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    store = TeamsPipelineStore(tmp_path / "teams-store.json")
    pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=store)
    created_notification = {
        "tenantId": "tenant-effect",
        "subscriptionId": "subscription-old",
        "changeType": "created",
        "resource": "communications/onlineMeetings/meeting-effect/transcripts/transcript-effect",
        "resourceData": {"id": "transcript-effect"},
    }
    updated_notification = {
        **created_notification,
        "subscriptionId": "subscription-renewed",
        "changeType": "updated",
    }
    created_event = TeamsPipelineStore.build_notification_receipt_key(created_notification)
    updated_event = TeamsPipelineStore.build_notification_receipt_key(updated_notification)
    assert created_event != updated_event

    metadata = {
        "notification_identity": {
            "artifact_type": "transcript",
            "artifact_id": "transcript-effect",
        }
    }
    created_job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-created",
        event_id=created_event,
        source_event_type="created",
        dedupe_key=created_event,
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-effect",
            tenant_id="tenant-effect",
            metadata=metadata,
        ),
    )
    updated_job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-updated",
        event_id=updated_event,
        source_event_type="updated",
        dedupe_key=updated_event,
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-effect",
            tenant_id="tenant-effect",
            metadata=metadata,
        ),
    )
    assert pipeline._build_sink_effect_key("teams", created_job) == pipeline._build_sink_effect_key(
        "teams", updated_job
    )
    remote_calls = 0

    async def remote_writer(existing_record=None):
        nonlocal remote_calls
        remote_calls += 1
        return {"message_id": "logical-effect"}

    await pipeline._write_sink_once(created_job, "teams", remote_writer)
    await pipeline._write_sink_once(updated_job, "teams", remote_writer)

    assert remote_calls == 1


@pytest.mark.anyio
async def test_cancelled_sink_call_marks_unknown_and_blocks_later_remote_retry(tmp_path):
    from plugins.teams_pipeline import pipeline as pipeline_module

    store = TeamsPipelineStore(tmp_path / "teams-store.json")
    pipeline = TeamsMeetingPipeline(graph_client=FakeGraphClient(), store=store)
    job = pipeline_module.TeamsMeetingPipelineJob(
        job_id="job-cancel",
        event_id="event-cancel",
        source_event_type="updated",
        dedupe_key="event-cancel",
        status="sending_teams",
        meeting_ref=pipeline_module.TeamsMeetingRef(
            meeting_id="meeting-cancel",
            tenant_id="tenant-cancel",
        ),
    )
    writer_started = asyncio.Event()
    wait_forever = asyncio.Event()
    remote_calls = 0

    async def cancellable_writer(existing_record=None):
        nonlocal remote_calls
        remote_calls += 1
        writer_started.set()
        await wait_forever.wait()
        return {"message_id": "must-not-exist"}

    task = asyncio.create_task(pipeline._write_sink_once(job, "teams", cancellable_writer))
    await writer_started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task

    sink_key = pipeline._build_sink_effect_key("teams", job)
    record = store.get_sink_record(sink_key)
    assert record is not None
    assert record["effect_status"] == "outcome_unknown"
    assert store.list_outcome_unknown_sink_records() == {sink_key: record}

    later_remote_calls = 0

    async def later_writer(existing_record=None):
        nonlocal later_remote_calls
        later_remote_calls += 1
        return {"message_id": "must-not-exist"}

    with pytest.raises(pipeline_module.TeamsPipelineSinkError, match="outcome is unknown"):
        await pipeline._write_sink_once(job, "teams", later_writer)
    assert remote_calls == 1
    assert later_remote_calls == 0
