import junction
import junction.config as config

import pytest


@pytest.fixture
def nginx() -> junction.config.Target:
    return {"namespace": "default", "name": "nginx"}


@pytest.fixture
def nginx_staging() -> junction.config.Target:
    return {"namespace": "default", "name": "nginx-staging"}


def test_check_basic_route(nginx):
    route: config.Route = {
        "vhost": nginx,
        "rules": [{"backends": [{**nginx, "port": 80}]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {},
    )

    assert matched_backend == {**nginx, "port": 80}


def test_check_basic_route_url_port(nginx):
    route: config.Route = {
        "vhost": nginx,
        "rules": [{"backends": [{**nginx, "port": 8888}]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:8910",
        {},
    )

    assert matched_backend == {**nginx, "port": 8888}


def test_check_basic_route_with_port(nginx):
    route: config.Route = {
        "vhost": {**nginx, "port": 1234},
        "rules": [{"backends": [{**nginx, "port": 1234}]}],
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


def test_check_empty_route(nginx):
    route: config.Route = {"vhost": nginx}

    (matching_route, matching_rule_idx, backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:1234",
        {},
    )

    assert matching_rule_idx is None
    assert {**nginx, "port": 1234} == backend


def test_check_empty_rules(nginx, nginx_staging):
    route: config.Route = {
        "vhost": nginx,
        "rules": [
            # /users hits staging
            {
                "matches": [{"path": {"value": "/users"}}],
                "backends": [{**nginx_staging, "port": 8910}],
            },
            # default to nginx
            {
                "retry": {
                    "attempts": 3,
                }
            },
        ],
    }

    (_, matching_rule_idx, backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:1234",
        {},
    )

    assert matching_rule_idx == 1
    assert {**nginx, "port": 1234} == backend

    (_, matching_rule_idx, backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local:1234/users",
        {},
    )

    assert matching_rule_idx == 0
    assert {**nginx_staging, "port": 8910} == backend


def test_check_retry_and_timeouts(nginx):
    retry: config.RouteRetry = {
        "attempts": 32,
        "backoff": 0.2,
        "codes": list(range(400, 499)),
    }
    timeouts: config.RouteTimeouts = {
        "request": 0.1,
    }

    route: config.Route = {
        "vhost": nginx,
        "rules": [
            {"backends": [{**nginx, "port": 80}], "retry": retry, "timeouts": timeouts}
        ],
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
    route: config.Route = {
        "vhost": nginx,
        "rules": [{"backends": [{**nginx_staging, "port": 80}]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local",
        {},
    )

    assert matched_backend == {**nginx_staging, "port": 80}


def test_no_fallthrough(nginx, nginx_staging):
    route: config.Route = {
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {
                "backends": [{**nginx, "port": 80}],
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
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [{**nginx, "port": 80}]},
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
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [{**nginx, "port": 80}]},
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
    assert [{**nginx, "port": 80}] == route["rules"][matched_rule_idx]["backends"]


def test_check_headers_match(nginx, nginx_staging):
    route: config.Route = {
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [{"headers": [{"name": "x-env", "value": "staging"}]}],
            },
            {"backends": [{**nginx, "port": 80}]},
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
    assert [{**nginx_staging, "port": 80}] == route["rules"][matched_rule_idx][
        "backends"
    ]


def test_check_path_matches_default(nginx, nginx_staging):
    route: config.Route = {
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [
                    {"path": {"type": "RegularExpression", "value": "foo.*bar"}}
                ],
            },
            {"backends": [{**nginx, "port": 80}]},
        ],
    }

    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local/barfoo",
        {},
    )

    assert {**nginx, "port": 80} == matched_backend
    assert [{**nginx, "port": 80}] == route["rules"][matched_rule_idx]["backends"]


def test_check_path_matches(nginx, nginx_staging):
    route: config.Route = {
        "vhost": nginx,
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [
                    {"path": {"type": "RegularExpression", "value": "foo.*bar"}}
                ],
            },
            {"backends": [{**nginx, "port": 80}]},
        ],
    }

    (_, matched_rule_idx, matched_backend) = junction.check_route(
        [route],
        "GET",
        "http://nginx.default.svc.cluster.local/fooblahbar",
        {},
    )

    assert {**nginx_staging, "port": 80} == matched_backend
    assert [{**nginx_staging, "port": 80}] == route["rules"][matched_rule_idx][
        "backends"
    ]
