"""Minimal HTTP client using only stdlib. No requests, no httpx, no aiohttp."""

from __future__ import annotations

import json
import ssl
import urllib.request
import urllib.error
from typing import Any, Dict, Optional


def _default_ssl_context() -> ssl.SSLContext:
    """Create an SSL context that works on macOS and Linux."""
    import certifi
    return ssl.create_default_context(cafile=certifi.where())


def _fallback_ssl_context() -> ssl.SSLContext:
    """Fallback SSL context when certifi is not available."""
    try:
        # Try system certificates first
        ctx = ssl.create_default_context()
        return ctx
    except Exception:
        # Last resort: unverified (development only)
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return ctx


def _get_ssl_context() -> ssl.SSLContext:
    """Get the best available SSL context."""
    try:
        return _default_ssl_context()
    except (ImportError, Exception):
        return _fallback_ssl_context()


def post_json(
    url: str,
    body: dict,
    headers: Optional[Dict[str, str]] = None,
    timeout: float = 120,
) -> dict:
    """POST JSON to a URL and return the parsed JSON response.

    Uses only ``urllib.request`` — zero dependencies.

    Raises:
        urllib.error.HTTPError: On non-2xx responses (includes response body).
        urllib.error.URLError: On connection errors.
        json.JSONDecodeError: On invalid JSON response.
        TimeoutError: On timeout.
    """
    data = json.dumps(body).encode("utf-8")
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)

    req = urllib.request.Request(url, data=data, headers=hdrs, method="POST")

    ssl_ctx = _get_ssl_context()
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=ssl_ctx) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        # Read the error body for debugging
        error_body = ""
        try:
            error_body = e.read().decode("utf-8")
        except Exception:
            pass
        raise type(e)(
            e.url, e.code, f"{e.reason}: {error_body}", e.headers, e.fp
        ) from None
