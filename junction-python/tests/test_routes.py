import junction
import junction.config as config

import pytest


@pytest.fixture
def nginx() -> junction.config.TargetService:
    return {"namespace": "default", "name": "nginx"}


@pytest.fixture
def nginx_staging() -> junction.config.TargetService:
    return {"namespace": "default", "name": "nginx-staging"}


def test_check_basic_route(nginx):
    route: config.Route = {"target": nginx, "rules": [{"backends": [nginx]}]}

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {},
    )

    assert matched_backend == {**nginx, "port": 80}


def test_check_basic_route_url_port(nginx):
    route: config.Route = {"target": nginx, "rules": [{"backends": [nginx]}]}

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:8910",
        {},
    )

    assert matched_backend == {**nginx, "port": 8910}


def test_check_basic_route_with_port(nginx):
    route: config.Route = {
        "target": {**nginx, "port": 1234},
        "rules": [{"backends": [nginx]}],
    }

    # explicitly specifying a different port should work
    with pytest.raises(RuntimeError, match="no routing info is available"):
        (_, _, matched_backend) = junction.check_route(
            [route],
            "GET",
            "http://nginx.default.svc.cluster.local:5678",
            {},
        )

    # explicitly specifying the right port should work
    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:1234",
        {},
    )
    assert matched_backend == {**nginx, "port": 1234}


def test_check_retry_and_timeouts(nginx):
    retry: config.RouteRetry = {
        "attempts": 32,
        "backoff": "200ms",
        "codes": list(range(400, 499)),
    }
    timeouts: config.RouteTimeouts = {
        "request": "100ms",
    }

    route: config.Route = {
        "target": nginx,
        "rules": [{"backends": [nginx], "retry": retry, "timeouts": timeouts}],
    }

    (matching_route, matching_rule_idx, _) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {},
    )

    assert timeouts == matching_route["rules"][matching_rule_idx]["timeouts"]
    assert retry == matching_route["rules"][matching_rule_idx]["retry"]


def test_check_redirect_route(nginx, nginx_staging):
    route: config.Route = {"target": nginx, "rules": [{"backends": [nginx_staging]}]}

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {},
    )

    assert matched_backend == {**nginx_staging, "port": 80}


def test_no_fallthrough(nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {
                "backends": [nginx],
                "matches": [{"headers": [{"name": "x-env", "value": "prod"}]}],
            },
        ],
    }

    with pytest.raises(RuntimeError, match="no routing info is available"):
        junction.check_route(
            [route],
            "GET",
            "http://something-else.default.svc.cluster.local",
            {},
        )


def test_no_target(nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [nginx]},
        ],
    }

    with pytest.raises(RuntimeError, match="no routing info is available"):
        junction.check_route(
            [route],
            "GET",
            "http://something-else.default.svc.cluster.local",
            {},
        )


@pytest.mark.parametrize(
    "headers",
    [
        {},
        {"x-something-else": "potato"},
        {"x-env": "prod"},
    ],
)
def test_check_headers_matches_default(headers, nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [nginx]},
        ],
    }

    # match without headers
    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        headers,
    )

    assert {**nginx, "port": 80} == matched_backend
    assert [nginx] == route["rules"][matched_rule_idx]["backends"]


def test_check_headers_match(nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [nginx]},
        ],
    }

    # match without headers
    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {"x-env": "staging"},
    )

    assert {**nginx_staging, "port": 80} == matched_backend
    assert [nginx_staging] == route["rules"][matched_rule_idx]["backends"]


def test_check_path_matches_default(nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [
                    {"path": {"type": "RegularExpression", "value": "foo.*bar"}}
                ],
            },
            {"backends": [nginx]},
        ],
    }

    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local/barfoo",
        {},
    )

    assert {**nginx, "port": 80} == matched_backend
    assert [nginx] == route["rules"][matched_rule_idx]["backends"]


def test_check_path_matches(nginx, nginx_staging):
    route: config.Route = {
        "target": nginx,
        "rules": [
            {
                "backends": [nginx_staging],
                "matches": [
                    {"path": {"type": "RegularExpression", "value": "foo.*bar"}}
                ],
            },
            {"backends": [nginx]},
        ],
    }

    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local/fooblahbar",
        {},
    )

    assert {**nginx_staging, "port": 80} == matched_backend
    assert [nginx_staging] == route["rules"][matched_rule_idx]["backends"]
