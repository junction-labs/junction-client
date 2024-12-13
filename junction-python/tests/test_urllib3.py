from urllib3 import Retry

from junction.urllib3 import _configure_retries
from junction import RetryPolicy


def retry_equals(r1: Retry, r2: Retry):
    return (r1 is r2) or (r1.__dict__ == r2.__dict__)


def test_retry_policy_only():
    expected = Retry(total=1, status_forcelist=[501, 503], backoff_factor=0.5)
    (actual, redirect_retries) = _configure_retries(
        retries=None, policy=RetryPolicy(codes=[501, 503], attempts=2, backoff=0.5)
    )

    assert retry_equals(expected, actual)
    assert redirect_retries == 10


def test_retries_only():
    expected = Retry(total=123, redirect=789, connect=123)
    (actual, redirect_retries) = _configure_retries(
        retries=expected,
        policy=None,
    )

    assert retry_equals(expected, actual)
    assert redirect_retries == 789


def test_merge_retries_and_policy():
    expected = Retry(total=123, redirect=789, connect=123)
    policy = RetryPolicy(codes=[501, 503], attempts=2, backoff=0.5)

    (actual, redirect_retries) = _configure_retries(
        retries=expected,
        policy=policy,
    )

    assert retry_equals(expected, actual)
    assert redirect_retries == 789


def test_merge_default_retries_and_policy():
    default_retry = Retry.from_int(0)
    policy = RetryPolicy(codes=[501, 503], attempts=2, backoff=0.5)

    (actual, redirect_retries) = _configure_retries(
        retries=default_retry,
        policy=policy,
    )

    assert retry_equals(
        Retry(total=1, status_forcelist=[501, 503], backoff_factor=0.5),
        actual,
    )
    assert redirect_retries == 10
