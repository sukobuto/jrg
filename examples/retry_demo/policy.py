"""Small search corpus with a real retry policy and incidental matches."""


def should_retry(status_code: int, attempts: int) -> bool:
    """Retry temporary HTTP failures within a fixed attempt budget."""
    # Client errors are generally permanent; replaying them would waste the budget.
    return attempts < 3 and (status_code == 429 or status_code >= 500)
