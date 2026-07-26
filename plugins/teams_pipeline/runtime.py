"""Gateway runtime wiring for the Teams meeting pipeline plugin."""

from __future__ import annotations

import logging
from typing import Any

from gateway.config import Platform
from plugins.teams_pipeline.pipeline import LinearWriter, NotionWriter, TeamsMeetingPipeline
from plugins.teams_pipeline.store import TeamsPipelineStore, resolve_teams_pipeline_store_path
from plugins.teams_pipeline.subscriptions import build_graph_client

logger = logging.getLogger(__name__)


class TeamsPipelineRuntimeError(RuntimeError):
    """Raised when enabled Teams pipeline delivery cannot be constructed."""


def _load_teams_summary_writer():
    from plugins.platforms.teams.adapter import TeamsSummaryWriter

    return TeamsSummaryWriter


def _teams_delivery_is_configured(teams_extra: dict[str, Any], teams_delivery: dict[str, Any]) -> bool:
    delivery_mode = str(
        teams_delivery.get("mode")
        or teams_delivery.get("delivery_mode")
        or teams_extra.get("delivery_mode")
        or ""
    ).strip().lower()

    if delivery_mode == "incoming_webhook":
        return bool(
            teams_delivery.get("incoming_webhook_url")
            or teams_extra.get("incoming_webhook_url")
        )
    if delivery_mode == "graph":
        chat_id = teams_delivery.get("chat_id") or teams_extra.get("chat_id")
        team_id = teams_delivery.get("team_id") or teams_extra.get("team_id")
        channel_id = teams_delivery.get("channel_id") or teams_extra.get("channel_id")
        return bool(chat_id or (team_id and channel_id))

    return False


def build_pipeline_runtime_config(gateway_config: Any) -> dict[str, Any]:
    """Build pipeline config from gateway platform config.

    Pipeline-specific knobs live under ``teams.extra.meeting_pipeline`` while
    Teams delivery continues to source its target details from the existing
    Teams platform config.
    """

    teams_config = gateway_config.platforms.get(Platform("teams"))
    teams_extra = dict((teams_config.extra or {}) if teams_config else {})
    pipeline_config = dict(teams_extra.get("meeting_pipeline") or {})

    if teams_config and teams_config.enabled:
        teams_delivery = dict(pipeline_config.get("teams_delivery") or {})

        delivery_mode = str(teams_extra.get("delivery_mode") or "").strip()
        if delivery_mode:
            teams_delivery["mode"] = delivery_mode

        for key in (
            "incoming_webhook_url",
            "access_token",
            "team_id",
            "channel_id",
            "chat_id",
        ):
            value = teams_extra.get(key)
            if value not in (None, ""):
                teams_delivery[key] = value

        if teams_delivery:
            teams_delivery["enabled"] = _teams_delivery_is_configured(teams_extra, teams_delivery)
            pipeline_config["teams_delivery"] = teams_delivery

    return pipeline_config


def build_pipeline_runtime(gateway: Any) -> TeamsMeetingPipeline:
    teams_sender = None
    teams_config = gateway.config.platforms.get(Platform("teams"))
    pipeline_config = build_pipeline_runtime_config(gateway.config)
    notion_config = pipeline_config.get("notion")
    linear_config = pipeline_config.get("linear")
    notion_writer = NotionWriter() if isinstance(notion_config, dict) and notion_config.get("enabled") else None
    linear_writer = LinearWriter() if isinstance(linear_config, dict) and linear_config.get("enabled") else None
    teams_delivery = dict(pipeline_config.get("teams_delivery") or {})
    if teams_delivery.get("enabled"):
        if teams_config is None or not teams_config.enabled:
            raise TeamsPipelineRuntimeError(
                "Teams delivery is enabled but the Teams platform is not enabled."
            )
        try:
            teams_sender = _load_teams_summary_writer()(platform_config=teams_config)
        except ImportError as exc:
            raise TeamsPipelineRuntimeError(
                "Teams delivery is enabled but TeamsSummaryWriter is unavailable."
            ) from exc

    return TeamsMeetingPipeline(
        graph_client=build_graph_client(),
        store=TeamsPipelineStore(resolve_teams_pipeline_store_path()),
        config=pipeline_config,
        notion_writer=notion_writer,
        linear_writer=linear_writer,
        teams_sender=teams_sender,
    )


def bind_gateway_runtime(gateway: Any) -> bool:
    """Attach the Teams pipeline runtime to the msgraph webhook adapter."""

    adapter = gateway.adapters.get(Platform.MSGRAPH_WEBHOOK)
    if adapter is None:
        return False

    if getattr(gateway, "_teams_pipeline_runtime", None) is not None:
        return True

    try:
        runtime = build_pipeline_runtime(gateway)
    except Exception as exc:
        error_message = str(exc)
        gateway._teams_pipeline_runtime_error = error_message
        logger.error("Teams pipeline runtime startup failed: %s", error_message)
        raise

    async def _schedule(notification: dict[str, Any], event: Any) -> None:
        await runtime.run_notification(notification)

    adapter.set_notification_scheduler(_schedule)
    gateway._teams_pipeline_runtime = runtime
    gateway._teams_pipeline_runtime_error = None
    return True
