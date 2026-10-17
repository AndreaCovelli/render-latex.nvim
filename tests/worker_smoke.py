"""Exercise a built worker without Neovim or third-party Python packages."""

import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


def smoke(binary, cache_root, windows_fallback=False):
    env = os.environ.copy()
    for key in list(env):
        if key.upper() in {"RENDER_LATEX_CACHE_DIR", "XDG_CACHE_HOME", "HOME"}:
            del env[key]
    if windows_fallback:
        env["LOCALAPPDATA" if os.name == "nt" else "LocalAppData"] = str(cache_root)
        expected_cache = cache_root / "render-latex" / "worker"
    else:
        env["RENDER_LATEX_CACHE_DIR"] = str(cache_root)
        expected_cache = cache_root

    requests = [
        {"id": 1, "method": "ping"},
        {"id": 2, "method": "render", "params": {"formula": r"\frac{1}{2}+\sqrt{x}"}},
        {"id": 3, "method": "render", "params": {"formula": r"\renderLatexUnknownCommand"}},
        {"id": 4, "method": "render", "params": {"formula": r"\slashed{p}+\centernot\implies"}},
        {"id": 5, "method": "shutdown"},
    ]
    frames = []
    for request in requests:
        body = json.dumps(request).encode("utf-8")
        frames.append(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body)
    result = subprocess.run(
        [str(binary)], input=b"".join(frames), capture_output=True, env=env, timeout=30
    )
    if result.returncode != 0:
        raise RuntimeError(f"worker exited {result.returncode}: {result.stderr.decode(errors='replace')}")

    responses = []
    remaining = result.stdout
    while remaining:
        header, separator, body = remaining.partition(b"\r\n\r\n")
        if not separator or not header.startswith(b"Content-Length: "):
            raise RuntimeError("worker returned an invalid response header")
        length = int(header.removeprefix(b"Content-Length: "))
        if len(body) < length:
            raise RuntimeError("worker returned an incomplete response")
        responses.append(json.loads(body[:length]))
        remaining = body[length:]
    assert [item["id"] for item in responses] == [1, 2, 3, 4, 5], responses
    assert responses[0]["result"]["diagnostics"] == "pong", responses[0]
    assert responses[2]["error"]["message"], responses[2]
    assert responses[4]["error"] is None, responses[4]
    for index in (1, 3):
        response = responses[index]
        assert response["error"] is None, response
        value = response["result"]
        png_path = Path(value["png_path"])
        assert png_path.parent == expected_cache, value
        png = png_path.read_bytes()
        assert png.startswith(b"\x89PNG\r\n\x1a\n"), value
        size = struct.unpack(">II", png[16:24])
        assert size == (value["width_px"], value["height_px"]), value
        assert size[0] > 0 and size[1] > 0, value


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: python tests/worker_smoke.py PATH_TO_WORKER")
    executable = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="render-latex-smoke-") as directory:
        root = Path(directory)
        smoke(executable, root / "explicit-cache")
        smoke(executable, root / "local-app-data", windows_fallback=True)
    print("Worker smoke passed: framing, rendering, error recovery, shutdown, and Windows cache fallback")
