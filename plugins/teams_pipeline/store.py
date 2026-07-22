"""Durable, privacy-bounded local state for the Teams pipeline plugin."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import tempfile
import threading
from contextlib import contextmanager
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable, Dict, Iterator, Optional
from urllib.parse import unquote, urlsplit

from hermes_constants import get_hermes_home

try:  # POSIX hosts use flock; Windows uses msvcrt byte-range locks.
    import fcntl
except ImportError:  # pragma: no cover - exercised on non-POSIX hosts.
    fcntl = None  # type: ignore[assignment]

try:  # pragma: no cover - exercised on Windows.
    import msvcrt
except ImportError:  # pragma: no cover - exercised on non-Windows hosts.
    msvcrt = None  # type: ignore[assignment]


DEFAULT_TEAMS_PIPELINE_STORE_FILENAME = "teams_pipeline_store.json"
PYTHON_TEAMS_PIPELINE_STORE_FORMAT = "hermes.teams_pipeline.python.v1"
PYTHON_TEAMS_PIPELINE_STORE_RUNTIME = "python"

DEFAULT_JOB_RETENTION_MAX_COUNT = 64
DEFAULT_JOB_RETENTION_MAX_BYTES = 512 * 1024
DEFAULT_JOB_RETENTION_TTL_SECONDS = 7 * 24 * 60 * 60
DEFAULT_RECEIPT_RETENTION_TTL_SECONDS = 7 * 24 * 60 * 60


_STATE_SECTIONS = (
    "subscriptions",
    "notification_receipts",
    "event_timestamps",
    "jobs",
    "sink_records",
)
_EVICTABLE_JOB_STATUSES = {"completed", "failed"}
_ARTIFACT_COLLECTIONS = {
    "transcripts": "transcript",
    "recordings": "recording",
}


class TeamsPipelineStoreError(RuntimeError):
    """Raised when durable Teams pipeline state cannot be safely used."""


class TeamsNotificationIdentityError(ValueError):
    """Raised when a Graph notification has no trustworthy stable identity."""

class _EffectReservation:
    """A held per-effect OS lock, released only after sink outcome persistence."""

    def __init__(
        self,
        *,
        reserved: bool,
        blocked: bool,
        record: dict[str, Any] | None,
        release: Callable[[], None] | None = None,
    ) -> None:
        self.reserved = reserved
        self.blocked = blocked
        self.record = deepcopy(record) if isinstance(record, dict) else None
        self._release = release

    @property
    def effect_status(self) -> str:
        return str((self.record or {}).get("effect_status") or "").strip()

    def release(self) -> None:
        release, self._release = self._release, None
        if release is not None:
            release()


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)


def _utc_now_iso() -> str:
    return _utc_now().isoformat()


def _parse_iso_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _normalized_identifier(value: Any, *, lower: bool = False) -> str | None:
    if value is None:
        return None
    normalized = str(value).strip()
    if not normalized:
        return None
    return normalized.lower() if lower else normalized


def _first_identifier(mapping: dict[str, Any], *keys: str, lower: bool = False) -> str | None:
    for key in keys:
        candidate = _normalized_identifier(mapping.get(key), lower=lower)
        if candidate:
            return candidate
    return None


def _segment_name_and_id(segment: str) -> tuple[str, str | None]:
    match = re.fullmatch(r"([A-Za-z][A-Za-z0-9._-]*)(?:\((.*)\))?", segment.strip())
    if not match:
        return segment.strip().lower(), None
    name, raw_id = match.groups()
    if raw_id is None:
        return name.lower(), None
    raw_id = raw_id.strip()
    if len(raw_id) >= 2 and raw_id[0] == raw_id[-1] == "'":
        raw_id = raw_id[1:-1].replace("''", "'")
    return name.lower(), _normalized_identifier(raw_id)


def _graph_resource_path(value: Any) -> list[str]:
    raw = _normalized_identifier(value)
    if not raw:
        return []
    parsed = urlsplit(raw)
    path = parsed.path if parsed.scheme or parsed.netloc else raw.split("?", 1)[0].split("#", 1)[0]
    raw_parts = [part for part in path.split("/") if part]
    parts = [unquote(part).strip() for part in raw_parts if unquote(part).strip()]
    for index, part in enumerate(parts):
        if _segment_name_and_id(part)[0] == "communications":
            return parts[index:]
    return parts


def _parse_graph_resource(value: Any) -> dict[str, str | None]:
    """Parse documented slash and parenthesized Microsoft Graph resource forms."""

    parts = _graph_resource_path(value)
    parsed: dict[str, str | None] = {
        "meeting_id": None,
        "artifact_type": None,
        "artifact_id": None,
        "resource": None,
    }
    if not parts:
        return parsed

    names = [_segment_name_and_id(part)[0] for part in parts]
    if "onlinemeetings" in names:
        online_index = names.index("onlinemeetings")
        _online_name, inline_meeting_id = _segment_name_and_id(parts[online_index])
        cursor = online_index + 1
        artifact_type: str | None = None
        artifact_id: str | None = None
        meeting_id = inline_meeting_id

        if meeting_id is None and cursor < len(parts):
            next_name, next_inline_id = _segment_name_and_id(parts[cursor])
            if next_name == "getalltranscripts":
                artifact_type = "transcript"
                cursor += 1
            elif next_name == "getallrecordings":
                artifact_type = "recording"
                cursor += 1
            elif next_name not in _ARTIFACT_COLLECTIONS:
                meeting_id = next_inline_id or _normalized_identifier(parts[cursor])
                cursor += 1

        if meeting_id is not None and cursor < len(parts):
            collection_name, inline_artifact_id = _segment_name_and_id(parts[cursor])
            artifact_type = _ARTIFACT_COLLECTIONS.get(collection_name)
            if artifact_type:
                cursor += 1
                if inline_artifact_id:
                    artifact_id = inline_artifact_id
                elif cursor < len(parts):
                    following_name, following_inline_id = _segment_name_and_id(parts[cursor])
                    if following_name not in _ARTIFACT_COLLECTIONS:
                        artifact_id = following_inline_id or _normalized_identifier(parts[cursor])

        parsed.update(
            {
                "meeting_id": meeting_id,
                "artifact_type": artifact_type,
                "artifact_id": artifact_id,
                "resource": _canonical_graph_resource(meeting_id, artifact_type, artifact_id),
            }
        )
        return parsed

    if "callrecords" in names:
        call_index = names.index("callrecords")
        _call_name, inline_call_id = _segment_name_and_id(parts[call_index])
        call_id = inline_call_id
        if call_id is None and call_index + 1 < len(parts):
            call_id = _normalized_identifier(parts[call_index + 1])
        parsed.update(
            {
                "artifact_type": "call_record" if call_id else None,
                "artifact_id": call_id,
                "resource": f"communications/callRecords/{call_id}" if call_id else None,
            }
        )
    return parsed


def _canonical_graph_resource(
    meeting_id: str | None,
    artifact_type: str | None,
    artifact_id: str | None,
) -> str | None:
    if not meeting_id:
        return None
    resource = f"communications/onlineMeetings/{meeting_id}"
    if artifact_type and artifact_id:
        collection = "transcripts" if artifact_type == "transcript" else "recordings"
        if artifact_type == "call_record":
            return f"{resource}/callRecords/{artifact_id}"
        return f"{resource}/{collection}/{artifact_id}"
    return resource


def _coalesce_identity_field(
    name: str,
    *values: str | None,
) -> str | None:
    present = [value for value in values if value]
    if not present:
        return None
    first = present[0]
    if any(value != first for value in present[1:]):
        raise TeamsNotificationIdentityError(f"Mismatched Graph notification {name}.")
    return first


def _normalize_graph_notification_identity(notification: dict[str, Any]) -> dict[str, str]:
    if not isinstance(notification, dict):
        raise TeamsNotificationIdentityError("Graph notification must be an object.")

    resource_data = notification.get("resourceData") or notification.get("resource_data") or {}
    if not isinstance(resource_data, dict):
        raise TeamsNotificationIdentityError("Graph notification resourceData must be an object.")

    resource_identity = _parse_graph_resource(notification.get("resource"))
    odata_identity = _parse_graph_resource(
        resource_data.get("@odata.id") or resource_data.get("odata_id") or resource_data.get("odataId")
    )
    meeting_id = _coalesce_identity_field(
        "meeting ID",
        resource_identity.get("meeting_id"),
        odata_identity.get("meeting_id"),
        _first_identifier(resource_data, "meetingId", "meeting_id", "onlineMeetingId", "online_meeting_id"),
        _first_identifier(notification, "meetingId", "meeting_id", "onlineMeetingId", "online_meeting_id"),
    )
    artifact_type = _coalesce_identity_field(
        "artifact type",
        resource_identity.get("artifact_type"),
        odata_identity.get("artifact_type"),
    )
    explicit_artifact_type: str | None = None
    explicit_artifact_id: str | None = None
    for key, candidate_type in (
        ("transcriptId", "transcript"),
        ("transcript_id", "transcript"),
        ("recordingId", "recording"),
        ("recording_id", "recording"),
        ("callRecordId", "call_record"),
        ("call_record_id", "call_record"),
        ("artifactId", "artifact"),
        ("artifact_id", "artifact"),
    ):
        candidate = _normalized_identifier(resource_data.get(key))
        if candidate:
            explicit_artifact_type = _coalesce_identity_field(
                "artifact type", explicit_artifact_type, candidate_type
            )
            explicit_artifact_id = _coalesce_identity_field(
                "artifact ID", explicit_artifact_id, candidate
            )

    artifact_type = _coalesce_identity_field("artifact type", artifact_type, explicit_artifact_type)
    artifact_id = _coalesce_identity_field(
        "artifact ID",
        resource_identity.get("artifact_id"),
        odata_identity.get("artifact_id"),
        explicit_artifact_id,
    )
    resource_data_id = _first_identifier(resource_data, "id")
    if resource_data_id:
        if artifact_id:
            artifact_id = _coalesce_identity_field("artifact ID", artifact_id, resource_data_id)
        elif artifact_type:
            artifact_id = resource_data_id
        elif meeting_id:
            meeting_id = _coalesce_identity_field("meeting ID", meeting_id, resource_data_id)

    if artifact_type and not artifact_id:
        raise TeamsNotificationIdentityError("Graph notification artifact has no stable artifact ID.")
    if artifact_id and not artifact_type:
        raise TeamsNotificationIdentityError("Graph notification artifact has no stable artifact type.")
    if not meeting_id:
        raise TeamsNotificationIdentityError("Graph notification has no stable meeting ID.")

    change_type = _first_identifier(notification, "changeType", "change_type", lower=True)
    if not change_type:
        raise TeamsNotificationIdentityError("Graph notification has no changeType.")

    tenant_id = _first_identifier(
        notification,
        "tenantId",
        "tenant_id",
        lower=True,
    ) or _first_identifier(resource_data, "tenantId", "tenant_id", lower=True) or ""
    subscription_id = _first_identifier(notification, "subscriptionId", "subscription_id", lower=True) or ""
    canonical_resource = _canonical_graph_resource(meeting_id, artifact_type, artifact_id)
    if not canonical_resource:
        raise TeamsNotificationIdentityError("Graph notification resource cannot be normalized.")

    return {
        "tenant_id": tenant_id,
        "subscription_id": subscription_id,
        "change_type": change_type,
        "resource": canonical_resource,
        "meeting_id": meeting_id,
        "artifact_type": artifact_type or "",
        "artifact_id": artifact_id or "",
    }


def _private_mode(stat_result: os.stat_result) -> bool:
    if os.name == "nt":
        return True
    return (stat.S_IMODE(stat_result.st_mode) & 0o077) == 0


def _owner_matches(stat_result: os.stat_result) -> bool:
    return not hasattr(os, "geteuid") or stat_result.st_uid == os.geteuid()


def _safe_json_value(value: Any) -> Any:
    """Copy JSON-shaped sink metadata while dropping volatile/private material."""

    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, list):
        return [_safe_json_value(item) for item in value]
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            normalized_key = str(key).replace("_", "").replace("-", "").lower()
            if any(
                marker in normalized_key
                for marker in (
                    "transcript",
                    "notification",
                    "clientstate",
                    "validationtoken",
                    "encryption",
                    "authorization",
                    "accesstoken",
                    "downloadurl",
                    "sourceurl",
                )
            ):
                continue
            cleaned[str(key)] = _safe_json_value(item)
        return cleaned
    return str(value)


def _minimal_meeting_ref(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    meeting_id = _first_identifier(value, "meeting_id", "meetingId", "id")
    if not meeting_id:
        return None
    result: dict[str, Any] = {"meeting_id": meeting_id}
    for output_key, keys in (
        ("organizer_user_id", ("organizer_user_id", "organizerUserId")),
        ("calendar_event_id", ("calendar_event_id", "calendarEventId")),
        ("thread_id", ("thread_id", "threadId")),
        ("tenant_id", ("tenant_id", "tenantId")),
    ):
        candidate = _first_identifier(value, *keys)
        if candidate:
            result[output_key] = candidate

    metadata = value.get("metadata")
    if isinstance(metadata, dict):
        minimal_metadata: dict[str, Any] = {}
        for key in ("subject", "startDateTime", "endDateTime", "call_record_id", "callRecordId"):
            candidate = metadata.get(key)
            if candidate is not None:
                minimal_metadata["call_record_id" if key == "callRecordId" else key] = _safe_json_value(candidate)
        identity = metadata.get("notification_identity")
        if isinstance(identity, dict):
            minimal_metadata["notification_identity"] = {
                key: str(identity.get(key) or "")
                for key in (
                    "tenant_id",
                    "subscription_id",
                    "change_type",
                    "resource",
                    "meeting_id",
                    "artifact_type",
                    "artifact_id",
                )
                if identity.get(key) is not None
            }
        if minimal_metadata:
            result["metadata"] = minimal_metadata
    return result


def _minimal_artifact(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    artifact_type = _normalized_identifier(value.get("artifact_type") or value.get("artifactType"))
    artifact_id = _first_identifier(value, "artifact_id", "artifactId", "id")
    if not artifact_type or not artifact_id:
        return None
    result: dict[str, Any] = {"artifact_type": artifact_type, "artifact_id": artifact_id}
    for output_key, keys in (
        ("display_name", ("display_name", "displayName", "name")),
        ("content_type", ("content_type", "contentType")),
        ("created_at", ("created_at", "createdAt", "createdDateTime")),
        ("available_at", ("available_at", "availableAt", "availableDateTime", "lastModifiedDateTime")),
        ("size_bytes", ("size_bytes", "sizeBytes", "size")),
    ):
        candidate = next((value.get(key) for key in keys if value.get(key) is not None), None)
        if candidate is not None:
            result[output_key] = _safe_json_value(candidate)
    return result


def _minimal_summary_payload(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    result: dict[str, Any] = {}
    meeting_ref = _minimal_meeting_ref(value.get("meeting_ref") or value.get("meetingRef"))
    if meeting_ref:
        result["meeting_ref"] = meeting_ref
    for output_key, keys in (
        ("title", ("title",)),
        ("start_time", ("start_time", "startTime")),
        ("end_time", ("end_time", "endTime")),
        ("participants", ("participants",)),
        ("summary", ("summary",)),
        ("key_decisions", ("key_decisions", "keyDecisions")),
        ("action_items", ("action_items", "actionItems")),
        ("risks", ("risks",)),
        ("call_metrics", ("call_metrics", "callMetrics")),
        ("confidence", ("confidence",)),
        ("confidence_notes", ("confidence_notes", "confidenceNotes")),
        ("notion_target", ("notion_target", "notionTarget")),
        ("linear_target", ("linear_target", "linearTarget")),
        ("teams_target", ("teams_target", "teamsTarget")),
    ):
        candidate = next((value.get(key) for key in keys if value.get(key) is not None), None)
        if candidate is not None:
            result[output_key] = _safe_json_value(candidate)
    artifacts = value.get("source_artifacts") or value.get("sourceArtifacts")
    if isinstance(artifacts, list):
        minimal_artifacts = [_minimal_artifact(item) for item in artifacts]
        result["source_artifacts"] = [item for item in minimal_artifacts if item is not None]
    return result or None


def _minimal_job_payload(payload: dict[str, Any]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for output_key, keys in (
        ("job_id", ("job_id", "jobId")),
        ("event_id", ("event_id", "eventId")),
        ("source_event_type", ("source_event_type", "sourceEventType")),
        ("dedupe_key", ("dedupe_key", "dedupeKey")),
        ("status", ("status",)),
        ("retry_count", ("retry_count", "retryCount")),
        ("created_at", ("created_at", "createdAt")),
        ("updated_at", ("updated_at", "updatedAt")),
        ("selected_artifact_strategy", ("selected_artifact_strategy", "selectedArtifactStrategy")),
        ("error_info", ("error_info", "errorInfo")),
    ):
        candidate = next((payload.get(key) for key in keys if payload.get(key) is not None), None)
        if candidate is not None:
            result[output_key] = _safe_json_value(candidate)
    meeting_ref = _minimal_meeting_ref(payload.get("meeting_ref") or payload.get("meetingRef"))
    if meeting_ref:
        result["meeting_ref"] = meeting_ref
    summary_payload = _minimal_summary_payload(payload.get("summary_payload") or payload.get("summaryPayload"))
    if summary_payload:
        result["summary_payload"] = summary_payload
    return result


def _minimal_receipt_metadata(payload: Any) -> dict[str, str]:
    if not isinstance(payload, dict):
        return {}
    identity = payload.get("notification_identity") or payload.get("identity")
    if not isinstance(identity, dict):
        return {}
    return {
        key: str(identity.get(key) or "")
        for key in (
            "tenant_id",
            "subscription_id",
            "change_type",
            "resource",
            "meeting_id",
            "artifact_type",
            "artifact_id",
        )
        if identity.get(key) is not None
    }


def resolve_teams_pipeline_store_path(path: str | Path | None = None) -> Path:
    if path is not None:
        explicit = str(path).strip()
        if explicit:
            return Path(explicit)

    env_path = os.getenv("MSGRAPH_WEBHOOK_STORE_PATH", "").strip()
    if env_path:
        return Path(env_path)

    return get_hermes_home() / DEFAULT_TEAMS_PIPELINE_STORE_FILENAME


class TeamsPipelineStore:
    """JSON-backed Teams pipeline state with atomic private copy-on-write updates."""

    def __init__(
        self,
        path: str | Path,
        *,
        job_retention_max_count: int = DEFAULT_JOB_RETENTION_MAX_COUNT,
        job_retention_max_bytes: int = DEFAULT_JOB_RETENTION_MAX_BYTES,
        job_retention_ttl_seconds: int = DEFAULT_JOB_RETENTION_TTL_SECONDS,
        receipt_retention_ttl_seconds: int = DEFAULT_RECEIPT_RETENTION_TTL_SECONDS,
    ) -> None:
        self.path = Path(path)
        self._lock = threading.RLock()
        self.job_retention_max_count = self._validated_nonnegative_limit(
            "job_retention_max_count", job_retention_max_count
        )
        self.job_retention_max_bytes = self._validated_nonnegative_limit(
            "job_retention_max_bytes", job_retention_max_bytes
        )
        self.job_retention_ttl_seconds = self._validated_nonnegative_limit(
            "job_retention_ttl_seconds", job_retention_ttl_seconds
        )
        self.receipt_retention_ttl_seconds = self._validated_nonnegative_limit(
            "receipt_retention_ttl_seconds", receipt_retention_ttl_seconds
        )
        self._state = self._empty_state()
        self._load()

    @staticmethod
    def _validated_nonnegative_limit(name: str, value: int) -> int:
        try:
            normalized = int(value)
        except (TypeError, ValueError) as exc:
            raise ValueError(f"{name} must be an integer.") from exc
        if normalized < 0:
            raise ValueError(f"{name} must not be negative.")
        return normalized

    @staticmethod
    def _empty_state() -> Dict[str, Dict[str, Any]]:
        return {section: {} for section in _STATE_SECTIONS}

    @classmethod
    def normalize_notification_identity(cls, notification: Dict[str, Any]) -> Dict[str, str]:
        """Return only stable Graph fields used for durable notification identity."""

        return _normalize_graph_notification_identity(notification)

    @classmethod
    def build_notification_receipt_key(cls, notification: Dict[str, Any]) -> str:
        """Build a stable key from Graph entity identity, never from its envelope."""

        identity = cls.normalize_notification_identity(notification)
        return cls.build_notification_receipt_key_from_identity(identity)

    @staticmethod
    def build_notification_receipt_key_from_identity(identity: Dict[str, Any]) -> str:
        canonical = {
            key: str(identity.get(key) or "")
            for key in (
                "tenant_id",
                "subscription_id",
                "change_type",
                "resource",
                "meeting_id",
                "artifact_type",
                "artifact_id",
            )
        }
        return "graph:" + json.dumps(canonical, sort_keys=True, separators=(",", ":"), ensure_ascii=True)

    def _load(self) -> None:
        with self._lock:
            self._state = self._read_state_from_disk()

    def _read_state_from_disk(self) -> Dict[str, Dict[str, Any]]:
        try:
            destination_stat = os.lstat(self.path)
        except FileNotFoundError:
            return self._empty_state()
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to inspect Teams pipeline store: {exc}") from exc

        self._validate_parent(create=False)
        self._validate_private_regular_file(destination_stat, "Teams pipeline store")
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        try:
            fd = os.open(self.path, flags)
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to open Teams pipeline store safely: {exc}") from exc
        try:
            opened_stat = os.fstat(fd)
            self._validate_private_regular_file(opened_stat, "Teams pipeline store")
            with os.fdopen(fd, "r", encoding="utf-8") as handle:
                fd = -1
                try:
                    data = json.load(handle, parse_constant=self._reject_json_constant)
                except (json.JSONDecodeError, ValueError) as exc:
                    raise TeamsPipelineStoreError("Teams pipeline store contains malformed JSON.") from exc
        finally:
            if fd != -1:
                os.close(fd)
        return self._validate_loaded_state(data)

    @staticmethod
    def _reject_json_constant(value: str) -> None:
        raise ValueError(f"Invalid JSON constant: {value}")

    def _validate_loaded_state(self, data: Any) -> Dict[str, Dict[str, Any]]:
        if not isinstance(data, dict):
            raise TeamsPipelineStoreError("Teams pipeline store must contain a JSON object.")
        store_format = data.get("store_format")
        runtime = data.get("runtime")
        if store_format is not None and not isinstance(store_format, str):
            raise TeamsPipelineStoreError("Teams pipeline store format marker must be a string.")
        if runtime is not None and not isinstance(runtime, str):
            raise TeamsPipelineStoreError("Teams pipeline store runtime marker must be a string.")
        normalized_format = str(store_format or "").strip().lower()
        normalized_runtime = str(runtime or "").strip().lower()
        if normalized_runtime == "rust" or ".rust." in normalized_format:
            raise TeamsPipelineStoreError("Refusing to open a Rust-marked Teams pipeline store.")
        if normalized_format and store_format != PYTHON_TEAMS_PIPELINE_STORE_FORMAT:
            raise TeamsPipelineStoreError("Teams pipeline store format is not compatible with Python runtime.")
        if normalized_runtime and runtime != PYTHON_TEAMS_PIPELINE_STORE_RUNTIME:
            raise TeamsPipelineStoreError("Teams pipeline store runtime is not Python.")

        state = self._empty_state()
        for section in _STATE_SECTIONS:
            value = data.get(section, {})
            if not isinstance(value, dict):
                raise TeamsPipelineStoreError(f"Teams pipeline store section {section!r} must be an object.")
            if section in {"subscriptions", "notification_receipts", "jobs", "sink_records"} and any(
                not isinstance(record, dict) for record in value.values()
            ):
                raise TeamsPipelineStoreError(f"Teams pipeline store section {section!r} contains malformed records.")
            state[section] = deepcopy(value)
        return state

    def _validate_parent(self, *, create: bool) -> Path:
        parent = self.path.parent
        if create:
            try:
                parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            except OSError as exc:
                raise TeamsPipelineStoreError(f"Unable to create Teams pipeline store parent: {exc}") from exc
        try:
            parent_stat = os.lstat(parent)
        except FileNotFoundError as exc:
            raise TeamsPipelineStoreError("Teams pipeline store parent does not exist.") from exc
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to inspect Teams pipeline store parent: {exc}") from exc
        if stat.S_ISLNK(parent_stat.st_mode) or not stat.S_ISDIR(parent_stat.st_mode):
            raise TeamsPipelineStoreError("Teams pipeline store parent must be a real directory.")
        if not _owner_matches(parent_stat) or not _private_mode(parent_stat):
            raise TeamsPipelineStoreError("Teams pipeline store parent must be owner-only.")
        return parent

    @staticmethod
    def _validate_private_regular_file(file_stat: os.stat_result, description: str) -> None:
        if stat.S_ISLNK(file_stat.st_mode) or not stat.S_ISREG(file_stat.st_mode):
            raise TeamsPipelineStoreError(f"{description} must be a regular file, not a symlink or special file.")
        if not _owner_matches(file_stat) or not _private_mode(file_stat):
            raise TeamsPipelineStoreError(f"{description} must be owner-only.")

    def _validate_existing_destination(self) -> None:
        try:
            destination_stat = os.lstat(self.path)
        except FileNotFoundError:
            return
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to inspect Teams pipeline store destination: {exc}") from exc
        self._validate_private_regular_file(destination_stat, "Teams pipeline store")

    @staticmethod
    def _lock_backend_available() -> bool:
        return fcntl is not None or msvcrt is not None

    def _open_private_lock_file(self, lock_path: Path, description: str) -> int:
        flags = os.O_CREAT | os.O_RDWR
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        try:
            fd = os.open(lock_path, flags, 0o600)
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to open {description} safely: {exc}") from exc
        try:
            if os.name != "nt":
                os.fchmod(fd, 0o600)
            self._validate_private_regular_file(os.fstat(fd), description)
            return fd
        except Exception:
            os.close(fd)
            raise

    @staticmethod
    def _acquire_os_lock(fd: int, *, blocking: bool) -> bool:
        if fcntl is not None:
            operation = fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB)
            try:
                fcntl.flock(fd, operation)
                return True
            except BlockingIOError:
                if not blocking:
                    return False
                raise
            except OSError as exc:
                raise TeamsPipelineStoreError(f"Unable to acquire Teams pipeline store lock: {exc}") from exc
        if msvcrt is not None:  # pragma: no cover - exercised on Windows.
            try:
                os.lseek(fd, 0, os.SEEK_SET)
                if os.fstat(fd).st_size == 0:
                    os.write(fd, b"\0")
                os.lseek(fd, 0, os.SEEK_SET)
                msvcrt.locking(fd, msvcrt.LK_LOCK if blocking else msvcrt.LK_NBLCK, 1)
                return True
            except OSError as exc:
                if not blocking:
                    return False
                raise TeamsPipelineStoreError(
                    f"Unable to acquire Teams pipeline store lock: {exc}"
                ) from exc
        raise TeamsPipelineStoreError(
            "Cross-process Teams pipeline store locking is unavailable; refusing mutation."
        )

    @staticmethod
    def _release_os_lock(fd: int) -> None:
        if fcntl is not None:
            fcntl.flock(fd, fcntl.LOCK_UN)
            return
        if msvcrt is not None:  # pragma: no cover - exercised on Windows.
            os.lseek(fd, 0, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
            return
        raise TeamsPipelineStoreError(
            "Cross-process Teams pipeline store locking is unavailable; refusing mutation."
        )

    def _effect_lock_path(self, sink_key: str) -> Path:
        parent = self._validate_parent(create=True)
        digest = hashlib.sha256(sink_key.encode("utf-8")).hexdigest()
        return parent / f".{self.path.name}.effect-{digest}.lock"

    def _try_hold_effect_lock(self, sink_key: str) -> Callable[[], None] | None:
        if not self._lock_backend_available():
            raise TeamsPipelineStoreError(
                "Cross-process Teams pipeline store locking is unavailable; refusing mutation."
            )
        fd = self._open_private_lock_file(
            self._effect_lock_path(sink_key), "Teams pipeline sink effect lock"
        )
        try:
            if not self._acquire_os_lock(fd, blocking=False):
                os.close(fd)
                return None
        except Exception:
            os.close(fd)
            raise

        def release() -> None:
            try:
                self._release_os_lock(fd)
            finally:
                os.close(fd)

        return release

    @contextmanager
    def _exclusive_mutation_lock(self) -> Iterator[None]:
        if not self._lock_backend_available():
            raise TeamsPipelineStoreError(
                "Cross-process Teams pipeline store locking is unavailable; refusing mutation."
            )
        parent = self._validate_parent(create=True)
        fd = self._open_private_lock_file(
            parent / f".{self.path.name}.lock", "Teams pipeline store lock"
        )
        locked = False
        try:
            self._acquire_os_lock(fd, blocking=True)
            locked = True
            yield
        finally:
            try:
                if locked:
                    self._release_os_lock(fd)
            finally:
                os.close(fd)

    def _persist_candidate(self, candidate: Dict[str, Dict[str, Any]]) -> None:
        parent = self._validate_parent(create=True)
        self._validate_existing_destination()
        try:
            document = {
                "store_format": PYTHON_TEAMS_PIPELINE_STORE_FORMAT,
                "runtime": PYTHON_TEAMS_PIPELINE_STORE_RUNTIME,
                **candidate,
            }
            encoded = json.dumps(document, indent=2, sort_keys=True, ensure_ascii=True).encode("utf-8")
        except (TypeError, ValueError) as exc:
            raise TeamsPipelineStoreError("Teams pipeline store state is not JSON-serializable.") from exc

        fd, tmp_name = tempfile.mkstemp(prefix=f".{self.path.name}.", suffix=".tmp", dir=str(parent))
        tmp_path = Path(tmp_name)
        try:
            if os.name != "nt":
                os.fchmod(fd, 0o600)
            with os.fdopen(fd, "wb") as handle:
                fd = -1
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
            self._validate_existing_destination()
            os.replace(tmp_path, self.path)
            tmp_path = None  # type: ignore[assignment]
            self._fsync_parent(parent)
        except OSError as exc:
            raise TeamsPipelineStoreError(f"Unable to persist Teams pipeline store safely: {exc}") from exc
        finally:
            if fd != -1:
                os.close(fd)
            if tmp_path is not None:
                try:
                    tmp_path.unlink()
                except FileNotFoundError:
                    pass

    @staticmethod
    def _fsync_parent(parent: Path) -> None:
        flags = os.O_RDONLY
        if hasattr(os, "O_DIRECTORY"):
            flags |= os.O_DIRECTORY
        fd = os.open(parent, flags)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def _mutate(self, mutation: Callable[[Dict[str, Dict[str, Any]]], Any]) -> Any:
        with self._lock:
            with self._exclusive_mutation_lock():
                current = self._read_state_from_disk()
                candidate = deepcopy(current)
                result = mutation(candidate)
                self._prune_state(candidate)
                if candidate != current:
                    self._persist_candidate(candidate)
                    self._state = candidate
                else:
                    self._state = current
                return deepcopy(result)

    def _prune_state(self, state: Dict[str, Dict[str, Any]]) -> None:
        now = _utc_now()
        receipts = state["notification_receipts"]
        for receipt_key, receipt in list(receipts.items()):
            if self._receipt_is_expired(receipt, now):
                del receipts[receipt_key]
        self._prune_jobs(state["jobs"], now)

    def _receipt_is_expired(self, receipt: dict[str, Any], now: datetime) -> bool:
        expires_at = _parse_iso_timestamp(receipt.get("expires_at"))
        if expires_at is None:
            received_at = _parse_iso_timestamp(receipt.get("received_at"))
            if received_at is None:
                return True
            expires_at = received_at + timedelta(seconds=self.receipt_retention_ttl_seconds)
            receipt["expires_at"] = expires_at.isoformat()
        return now >= expires_at

    @staticmethod
    def _is_retry_required(record: dict[str, Any]) -> bool:
        return str(record.get("status") or "").strip().lower() not in _EVICTABLE_JOB_STATUSES

    def _prune_jobs(self, jobs: Dict[str, Any], now: datetime) -> None:
        for job_id, record in list(jobs.items()):
            if not isinstance(record, dict) or self._is_retry_required(record):
                continue
            updated_at = _parse_iso_timestamp(record.get("updated_at") or record.get("created_at"))
            if updated_at is None or now >= updated_at + timedelta(seconds=self.job_retention_ttl_seconds):
                del jobs[job_id]

        def evictable_oldest() -> list[str]:
            candidates: list[tuple[datetime, str]] = []
            for job_id, record in jobs.items():
                if not isinstance(record, dict) or self._is_retry_required(record):
                    continue
                timestamp = _parse_iso_timestamp(record.get("updated_at") or record.get("created_at"))
                candidates.append((timestamp or datetime.min.replace(tzinfo=timezone.utc), job_id))
            return [job_id for _timestamp, job_id in sorted(candidates)]

        for job_id in evictable_oldest():
            if len(jobs) <= self.job_retention_max_count:
                break
            del jobs[job_id]

        def retained_size() -> int:
            return sum(
                len(json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8"))
                for record in jobs.values()
            )

        for job_id in evictable_oldest():
            if retained_size() <= self.job_retention_max_bytes:
                break
            if job_id in jobs:
                del jobs[job_id]

    def list_subscriptions(self) -> Dict[str, Dict[str, Any]]:
        with self._lock:
            return deepcopy(self._state["subscriptions"])

    def get_subscription(self, subscription_id: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            record = self._state["subscriptions"].get(subscription_id)
            return deepcopy(record) if isinstance(record, dict) else None

    def upsert_subscription(self, subscription_id: str, payload: Dict[str, Any]) -> Dict[str, Any]:
        subscription_id = str(subscription_id).strip()
        if not subscription_id:
            raise ValueError("subscription_id is required.")
        safe_payload = _safe_json_value(payload) if isinstance(payload, dict) else {}

        def mutation(state: Dict[str, Dict[str, Any]]) -> Dict[str, Any]:
            existing = state["subscriptions"].get(subscription_id, {})
            merged = {**existing, **deepcopy(safe_payload)}
            merged["subscription_id"] = subscription_id
            merged.setdefault("created_at", existing.get("created_at") or _utc_now_iso())
            merged["updated_at"] = _utc_now_iso()
            state["subscriptions"][subscription_id] = merged
            return merged

        return self._mutate(mutation)

    def delete_subscription(self, subscription_id: str) -> bool:
        def mutation(state: Dict[str, Dict[str, Any]]) -> bool:
            removed = state["subscriptions"].pop(subscription_id, None)
            return removed is not None

        return bool(self._mutate(mutation))

    def has_notification_receipt(self, receipt_key: str) -> bool:
        return self.get_notification_receipt(receipt_key) is not None

    def get_notification_receipt(self, receipt_key: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            receipt = self._state["notification_receipts"].get(receipt_key)
            if not isinstance(receipt, dict) or self._receipt_is_expired(receipt, _utc_now()):
                return None
            return deepcopy(receipt)

    def record_notification_receipt(
        self,
        receipt_key: str,
        payload: Optional[Dict[str, Any]] = None,
        *,
        received_at: Optional[str] = None,
    ) -> bool:
        safe_metadata = _minimal_receipt_metadata(payload)

        def mutation(state: Dict[str, Dict[str, Any]]) -> bool:
            receipts = state["notification_receipts"]
            existing = receipts.get(receipt_key)
            if isinstance(existing, dict) and not self._receipt_is_expired(existing, _utc_now()):
                return False
            if existing is not None:
                del receipts[receipt_key]
            timestamp = received_at or _utc_now_iso()
            timestamp_value = _parse_iso_timestamp(timestamp) or _utc_now()
            receipts[receipt_key] = {
                "received_at": timestamp,
                "expires_at": (timestamp_value + timedelta(seconds=self.receipt_retention_ttl_seconds)).isoformat(),
                **safe_metadata,
            }
            return True

        return bool(self._mutate(mutation))

    def update_notification_receipt(
        self,
        receipt_key: str,
        *,
        job_id: str | None = None,
        status: str | None = None,
        source_event_type: str | None = None,
        meeting_id: str | None = None,
        tenant_id: str | None = None,
        artifact_type: str | None = None,
        artifact_id: str | None = None,
    ) -> bool:
        updates = {
            key: value
            for key, value in {
                "job_id": job_id,
                "status": status,
                "source_event_type": source_event_type,
                "meeting_id": meeting_id,
                "tenant_id": tenant_id,
                "artifact_type": artifact_type,
                "artifact_id": artifact_id,
            }.items()
            if value is not None and str(value).strip()
        }

        def mutation(state: Dict[str, Dict[str, Any]]) -> bool:
            receipt = state["notification_receipts"].get(receipt_key)
            if not isinstance(receipt, dict) or self._receipt_is_expired(receipt, _utc_now()):
                return False
            receipt.update({key: str(value) for key, value in updates.items()})
            return True

        return bool(self._mutate(mutation))

    def record_event_timestamp(self, event_key: str, timestamp: Optional[str] = None) -> str:
        value = timestamp or _utc_now_iso()

        def mutation(state: Dict[str, Dict[str, Any]]) -> str:
            state["event_timestamps"][event_key] = value
            return value

        return str(self._mutate(mutation))

    def get_event_timestamp(self, event_key: str) -> Optional[str]:
        with self._lock:
            value = self._state["event_timestamps"].get(event_key)
            return str(value) if value is not None else None

    def stats(self) -> Dict[str, int]:
        with self._lock:
            return {section: len(self._state[section]) for section in _STATE_SECTIONS}

    def upsert_job(self, job_id: str, payload: Dict[str, Any]) -> Dict[str, Any]:
        job_id = str(job_id).strip()
        if not job_id:
            raise ValueError("job_id is required.")
        safe_payload = _minimal_job_payload(payload) if isinstance(payload, dict) else {}

        def mutation(state: Dict[str, Dict[str, Any]]) -> Dict[str, Any]:
            existing = state["jobs"].get(job_id, {})
            merged = {**existing, **deepcopy(safe_payload)}
            merged["job_id"] = job_id
            merged.setdefault("created_at", existing.get("created_at") or _utc_now_iso())
            merged["updated_at"] = _utc_now_iso()
            state["jobs"][job_id] = merged
            return merged

        return self._mutate(mutation)

    def get_job(self, job_id: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            record = self._state["jobs"].get(job_id)
            return deepcopy(record) if isinstance(record, dict) else None

    def list_jobs(self) -> Dict[str, Dict[str, Any]]:
        with self._lock:
            return deepcopy(self._state["jobs"])

    def upsert_sink_record(self, sink_key: str, payload: Dict[str, Any]) -> Dict[str, Any]:
        sink_key = str(sink_key).strip()
        if not sink_key:
            raise ValueError("sink_key is required.")
        safe_payload = _safe_json_value(payload) if isinstance(payload, dict) else {}

        def mutation(state: Dict[str, Dict[str, Any]]) -> Dict[str, Any]:
            existing = state["sink_records"].get(sink_key, {})
            merged = {**existing, **deepcopy(safe_payload)}
            merged["sink_key"] = sink_key
            merged.setdefault("created_at", existing.get("created_at") or _utc_now_iso())
            merged["updated_at"] = _utc_now_iso()
            state["sink_records"][sink_key] = merged
            return merged

        return self._mutate(mutation)

    def reserve_effect_started(
        self,
        sink_key: str,
        *,
        job_id: str,
        event_id: str,
        sink: str,
    ) -> _EffectReservation:
        """Reserve one remote effect while holding its OS lock through completion."""

        sink_key = str(sink_key).strip()
        if not sink_key:
            raise ValueError("sink_key is required.")
        release = self._try_hold_effect_lock(sink_key)
        if release is None:
            return _EffectReservation(reserved=False, blocked=True, record=None)
        reservation = {
            "effect_status": "effect_started",
            "effect_started_at": _utc_now_iso(),
            "job_id": str(job_id),
            "event_id": str(event_id),
            "sink": str(sink),
        }

        def mutation(state: Dict[str, Dict[str, Any]]) -> Dict[str, Any]:
            existing = state["sink_records"].get(sink_key)
            if isinstance(existing, dict):
                effect_status = str(existing.get("effect_status") or "").strip()
                if effect_status == "effect_started":
                    recovered = {
                        **existing,
                        "effect_status": "outcome_unknown",
                        "effect_recovery_reason": "effect_lock_released_before_outcome",
                        "updated_at": _utc_now_iso(),
                    }
                    state["sink_records"][sink_key] = recovered
                    return {"reserved": False, "record": recovered}
                if effect_status != "effect_not_started":
                    return {"reserved": False, "record": existing}
                prior_record = deepcopy(existing)
            else:
                prior_record = {}
            merged = {**prior_record, **reservation}
            merged["sink_key"] = sink_key
            merged.setdefault("created_at", prior_record.get("created_at") or _utc_now_iso())
            merged["updated_at"] = _utc_now_iso()
            state["sink_records"][sink_key] = merged
            return {"reserved": True, "record": prior_record}

        try:
            result = self._mutate(mutation)
        except Exception:
            release()
            raise
        if result["reserved"]:
            return _EffectReservation(
                reserved=True,
                blocked=False,
                record=result["record"],
                release=release,
            )
        release()
        return _EffectReservation(
            reserved=False,
            blocked=False,
            record=result["record"],
        )

    def get_sink_record(self, sink_key: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            record = self._state["sink_records"].get(sink_key)
            return deepcopy(record) if isinstance(record, dict) else None

    def list_outcome_unknown_sink_records(self) -> Dict[str, Dict[str, Any]]:
        """Return durable sink effects that require operator reconciliation."""

        with self._lock:
            return {
                sink_key: deepcopy(record)
                for sink_key, record in self._state["sink_records"].items()
                if isinstance(record, dict) and record.get("effect_status") == "outcome_unknown"
            }
