import junction
import junction.config as config

import pytest


@pytest.fixture
def nginx() -> junction.config.Service:
    return {"type": "kube", "namespace": "default", "name": "nginx"}


@pytest.fixture
def nginx_staging() -> junction.config.Service:
    return {"type": "kube", "namespace": "default", "name": "nginx-staging"}


def test_check_basic_route_url_port(nginx):
    route: config.Route = {
        "id": "test-route",
        "hostnames": [
            f"{nginx['name']}.{nginx['namespace']}.svc.cluster.local",
        ],
        "rules": [{"backends": [nginx]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local",
    )

    assert matched_backend == {**nginx, "port": 80}


def test_check_basic_route_url_port_with_ndots(nginx):
    search_config: config.SearchConfig = {
        "ndots": 5,
        "search": ["svc.cluster.local"],
    }
    route: config.Route = {
        "id": "test-route",
        "hostnames": [
            f"{nginx['name']}.{nginx['namespace']}.svc.cluster.local",
        ],
        "rules": [{"backends": [nginx]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route], "http://nginx.default", search_config=search_config
    )

    assert matched_backend == {**nginx, "port": 80}


def test_check_basic_route_backend_port(nginx):
    route: config.Route = {
        "id": "test-route",
        "hostnames": [
            f"{nginx['name']}.{nginx['namespace']}.svc.cluster.local",
        ],
        "rules": [{"backends": [{**nginx, "port": 8888}]}],
    }

    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local:8910",
    )

    assert matched_backend == {**nginx, "port": 8888}


def test_hostname_match(nginx):
    route: config.Route = {
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
        "rules": [
            {
                "backends": [{**nginx, "port": 80}],
            },
        ],
    }

    with pytest.raises(RuntimeError, match="no route matched"):
        junction.check_route(
            [route],
            "http://something-else.default.svc.cluster.local",
        )


def test_check_basic_route_ports_match(nginx):
    route: config.Route = {
        "id": "test-route",
        "hostnames": [
            f"{nginx['name']}.{nginx['namespace']}.svc.cluster.local",
        ],
        "ports": [1234],
        "rules": [{"backends": [{**nginx, "port": 1234}]}],
    }

    # explicitly specifying a different port should not work
    with pytest.raises(RuntimeError, match="no route matched"):
        (_, _, matched_backend) = junction.check_route(
            [route],
            "http://nginx.default.svc.cluster.local:5678",
        )

    # using the default http/https port should not work
    with pytest.raises(RuntimeError, match="no route matched"):
        (_, _, matched_backend) = junction.check_route(
            [route],
            "https://nginx.default.svc.cluster.local",
        )

    # explicitly specifying the right port should work
    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local:1234",
    )
    assert matched_backend == {**nginx, "port": 1234}


def test_check_empty_route(nginx):
    route: config.Route = {"id": "empty-route"}

    with pytest.raises(RuntimeError, match="no route matched"):
        _ = junction.check_route(
            [route],
            "http://nginx.default.svc.cluster.local:1234",
        )


def test_check_empty_rules(nginx, nginx_staging):
    route: config.Route = {
        "id": "test-route",
        "hostnames": [
            f"{nginx['name']}.{nginx['namespace']}.svc.cluster.local",
        ],
        "rules": [
            # /users hits staging
            {
                "matches": [{"path": {"value": "/users"}}],
                "backends": [{**nginx_staging, "port": 8910}],
            },
            # default to nginx
            {},
        ],
    }

    with pytest.raises(RuntimeError, match="invalid route"):
        _ = junction.check_route(
            [route],
            "http://nginx.default.svc.cluster.local:1234",
        )

    (_, matching_rule_idx, backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local:1234/users",
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
    route = config.Route(
        id="test-route",
        hostnames=["*.default.svc.cluster.local"],
        rules=[
            {"backends": [{**nginx, "port": 80}], "retry": retry, "timeouts": timeouts}
        ],
    )

    (matching_route, matching_rule_idx, _) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local",
    )

    assert timeouts == matching_route["rules"][matching_rule_idx]["timeouts"]
    assert retry == matching_route["rules"][matching_rule_idx]["retry"]


def test_check_redirect_route(nginx_staging):
    route = config.Route(
        id="test-route",
        hostnames=["*.default.svc.cluster.local"],
        rules=[{"backends": [{**nginx_staging, "port": 80}]}],
    )

    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local",
    )

    assert matched_backend == {**nginx_staging, "port": 80}


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
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
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
        "http://nginx.default.svc.cluster.local",
        headers=headers,
    )

    assert {**nginx, "port": 80} == matched_backend
    assert [{**nginx, "port": 80}] == route["rules"][matched_rule_idx]["backends"]


def test_check_method_match(nginx, nginx_staging):
    route: config.Route = {
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
        "rules": [
            {
                "backends": [{**nginx_staging, "port": 80}],
                "matches": [{"method": "POST"}],
            },
            {"backends": [{**nginx, "port": 80}]},
        ],
    }

    # match a POST
    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local",
        method="POST",
    )
    assert {**nginx_staging, "port": 80} == matched_backend

    # don't match a PUT
    (_, _, matched_backend) = junction.check_route(
        [route],
        "http://nginx.default.svc.cluster.local",
        method="PUT",
    )
    assert {**nginx, "port": 80} == matched_backend


def test_check_headers_match(nginx, nginx_staging):
    route: config.Route = {
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
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
        "http://nginx.default.svc.cluster.local",
        headers={"x-env": "staging"},
    )

    assert {**nginx_staging, "port": 80} == matched_backend
    assert [{**nginx_staging, "port": 80}] == route["rules"][matched_rule_idx][
        "backends"
    ]


def test_check_path_matches_default(nginx, nginx_staging):
    route: config.Route = {
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
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
        "http://nginx.default.svc.cluster.local/barfoo",
    )

    assert {**nginx, "port": 80} == matched_backend
    assert [{**nginx, "port": 80}] == route["rules"][matched_rule_idx]["backends"]


def test_check_path_matches(nginx, nginx_staging):
    route: config.Route = {
        "id": "test-route",
        "hostnames": ["nginx.default.svc.cluster.local"],
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
        "http://nginx.default.svc.cluster.local/fooblahbar",
    )

    assert {**nginx_staging, "port": 80} == matched_backend
    assert [{**nginx_staging, "port": 80}] == route["rules"][matched_rule_idx][
        "backends"
    ]
