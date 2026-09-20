"""Telemetry that mentions retry without deciding whether to retry."""


def retry_metric(attempts: int) -> str:
    """Format an attempt counter for display."""
    return f"retry_attempts={attempts}"
