"""NovaVM Sandbox — create and interact with microVM sandboxes via REST API."""

from __future__ import annotations

import base64
import json
import re
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass
from typing import Any, Dict, List, Optional


@dataclass
class ExecResult:
    """Result of executing a shell command in a sandbox."""

    stdout: str
    stderr: str
    exit_code: int


@dataclass
class Execution:
    """Result of running code in a sandbox."""

    text: str
    stdout: str
    stderr: str
    exit_code: int
    error: Optional[str] = None


# Helper script installed in the VM for stateful code execution.
# Blocks are separated by a sentinel line. All previous blocks run silently
# in 'exec' mode; only the last block runs in 'single' (interactive) mode
# so bare expressions auto-print. State lives in `ns` dict.
_REPL_HELPER = r'''
import sys, ast, io

SEP = '# ---NOVAVM_BLOCK---'
source = open('/tmp/_novavm_code.py').read()
parts = source.split('\n' + SEP + '\n')
prev = '\n'.join(parts[:-1]) if len(parts) > 1 else ''
current = parts[-1]

ns = {'__builtins__': __builtins__}

if prev:
    try:
        exec(compile(prev, '<novavm>', 'exec'), ns)
    except Exception:
        pass

old_stdout, old_stderr = sys.stdout, sys.stderr
sys.stdout = out_buf = io.StringIO()
sys.stderr = err_buf = io.StringIO()

try:
    tree = ast.parse(current)
    for node in tree.body:
        mod = ast.Interactive(body=[node])
        co = compile(mod, '<novavm>', 'single')
        exec(co, ns)
except Exception:
    import traceback
    traceback.print_exc()

sys.stdout, sys.stderr = old_stdout, old_stderr

output = out_buf.getvalue()
errors = err_buf.getvalue()
if output:
    sys.stdout.write(output)
if errors:
    sys.stderr.write(errors)
'''


def _b64_write_cmd(path: str, content: str) -> str:
    """Encode content as base64 and return a single-line shell command to decode it into a file."""
    encoded = base64.b64encode(content.encode()).decode()
    return f"echo {encoded} | base64 -d > {path}"


class Sandbox:
    """A NovaVM sandbox running inside a KVM microVM.

    Connects to the nova-daemon REST API over HTTP.

    Usage::

        from novavm import Sandbox

        with Sandbox(image="python:3.11-slim") as sb:
            sb.run_code("x = 1")
            result = sb.run_code("x += 1; x")
            print(result.text)  # 2

    Args:
        image: OCI image reference (default: python:3.11-slim).
        vcpus: Number of virtual CPUs.
        memory: Memory in MiB.
        name: Sandbox name (auto-generated if omitted).
        base_url: REST API base URL (default: http://localhost:9800).
        timeout: HTTP request timeout in seconds.
    """

    def __init__(
        self,
        image: str = "python:3.11-slim",
        *,
        vcpus: int = 1,
        memory: int = 256,
        name: Optional[str] = None,
        base_url: str = "http://localhost:9800",
        timeout: int = 30,
    ):
        self.image = image
        self.vcpus = vcpus
        self.memory = memory
        self.sandbox_id = name or f"novavm-{uuid.uuid4().hex[:8]}"
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self._started = False
        self._repl_installed = False
        self._code_blocks: List[str] = []

    # ── HTTP helpers ──────────────────────────────────────────

    def _request(
        self,
        method: str,
        path: str,
        body: Optional[Dict[str, Any]] = None,
    ) -> Dict[str, Any]:
        """Send an HTTP request to the REST API and return parsed JSON."""
        url = f"{self.base_url}{path}"
        data = json.dumps(body).encode() if body else None
        req = urllib.request.Request(
            url,
            data=data,
            headers={"Content-Type": "application/json"} if data else {},
            method=method,
        )
        try:
            resp = urllib.request.urlopen(req, timeout=self.timeout)
            raw = resp.read()
            return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                err_body = json.loads(raw)
            except (json.JSONDecodeError, ValueError):
                err_body = {"error": raw.decode("utf-8", errors="replace")}
            raise SandboxError(
                f"{method} {path} failed ({e.code}): {err_body.get('error', err_body)}"
            ) from e

    # ── Lifecycle ────────────────────────────────────────────────

    def start(self) -> "Sandbox":
        """Create and start the sandbox. Called automatically by __enter__."""
        self._request("POST", "/api/v1/sandboxes", {
            "sandbox_id": self.sandbox_id,
            "image": self.image,
            "vcpus": self.vcpus,
            "memory": self.memory,
        })
        self._started = True
        return self

    def stop(self) -> None:
        """Stop the sandbox."""
        if self._started:
            self._request("POST", f"/api/v1/sandboxes/{self.sandbox_id}/stop")
            self._started = False

    def destroy(self) -> None:
        """Stop and remove the sandbox."""
        try:
            self._request("DELETE", f"/api/v1/sandboxes/{self.sandbox_id}")
        except SandboxError:
            pass
        self._started = False

    @property
    def is_running(self) -> bool:
        """Check if the sandbox is running."""
        try:
            resp = self._request("GET", f"/api/v1/sandboxes/{self.sandbox_id}")
            return resp.get("state") == "Running"
        except SandboxError:
            return False

    # ── Command Execution ───────────────────────────────────────

    def exec(self, command: str, args: Optional[List[str]] = None) -> ExecResult:
        """Execute a shell command inside the sandbox.

        Args:
            command: The command to run (passed as a single shell string).
            args: Additional arguments (appended to command).

        Returns:
            ExecResult with stdout, stderr, and exit_code.
        """
        cmd = command
        if args:
            cmd = f"{command} {' '.join(args)}"
        try:
            resp = self._request(
                "POST",
                f"/api/v1/sandboxes/{self.sandbox_id}/exec",
                {"command": cmd},
            )
            return ExecResult(
                stdout=resp.get("stdout", ""),
                stderr=resp.get("stderr", ""),
                exit_code=resp.get("exit_code", 0),
            )
        except SandboxError as e:
            return ExecResult(stdout="", stderr=str(e), exit_code=1)

    def run_code(self, code: str) -> Execution:
        """Execute code with persistent state across calls.

        Maintains variable state between calls, just like a REPL.
        Bare expressions auto-print their value.

        Args:
            code: Python code to execute.

        Returns:
            Execution with text output.

        Example::

            sb.run_code("x = 1")
            result = sb.run_code("x += 1; x")
            print(result.text)  # 2
        """
        if not self._repl_installed:
            self._install_repl_helper()

        # Accumulate code blocks; the REPL helper re-executes all previous blocks
        # silently and only captures output from the latest block.
        self._code_blocks.append(code)
        full_code = "\n# ---NOVAVM_BLOCK---\n".join(self._code_blocks)
        self.exec(_b64_write_cmd("/tmp/_novavm_code.py", full_code))

        # Execute via the REPL helper.
        result = self.exec("python3 /tmp/_novavm_runner.py")

        # Clean serial output: strip \r, trailing newlines, and kernel noise.
        text = result.stdout.replace("\r\n", "\n").replace("\r", "").rstrip("\n")
        lines = text.split("\n")
        clean_lines = [l for l in lines if not re.match(r'^\[\s*\d+\.\d+\]', l.lstrip())]
        text = "\n".join(clean_lines).strip()

        error = result.stderr if result.exit_code != 0 else None

        return Execution(
            text=text,
            stdout=result.stdout,
            stderr=result.stderr,
            exit_code=result.exit_code,
            error=error,
        )

    # ── File Operations ─────────────────────────────────────────

    def write_file(self, path: str, content: str) -> None:
        """Write a file inside the sandbox."""
        self._request(
            "POST",
            f"/api/v1/sandboxes/{self.sandbox_id}/files/write",
            {"path": path, "content": content},
        )

    def read_file(self, path: str) -> str:
        """Read a file from the sandbox."""
        resp = self._request(
            "POST",
            f"/api/v1/sandboxes/{self.sandbox_id}/files/read",
            {"path": path},
        )
        return resp.get("content", "")

    # ── Context Manager ─────────────────────────────────────────

    def __enter__(self) -> "Sandbox":
        self.start()
        return self

    def __exit__(self, *exc) -> None:
        self.destroy()

    # ── Internals ───────────────────────────────────────────────

    def _install_repl_helper(self) -> None:
        """Install the REPL helper script in the VM via base64."""
        self.exec(_b64_write_cmd("/tmp/_novavm_runner.py", _REPL_HELPER))
        self._repl_installed = True

    def __repr__(self) -> str:
        state = "running" if self._started else "stopped"
        return f"Sandbox(id={self.sandbox_id!r}, image={self.image!r}, state={state!r})"


class SandboxError(Exception):
    """Error from NovaVM sandbox operations."""
