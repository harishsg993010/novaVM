import { randomBytes } from "crypto";

/** Encode content as base64 and return a single-line shell command to write it to a file. */
function b64WriteCmd(filePath: string, content: string): string {
  const encoded = Buffer.from(content).toString("base64");
  return `echo ${encoded} | base64 -d > ${filePath}`;
}

/** Options for creating a Sandbox. */
export interface SandboxOptions {
  /** OCI image reference (default: "python:3.11-slim"). */
  image?: string;
  /** Number of virtual CPUs (default: 1). */
  vcpus?: number;
  /** Memory in MiB (default: 256). */
  memory?: number;
  /** Sandbox name (auto-generated if omitted). */
  name?: string;
  /** REST API base URL (default: "http://localhost:9800"). */
  baseUrl?: string;
  /** HTTP request timeout in seconds (default: 30). */
  timeout?: number;
}

/** Result of executing a shell command. */
export interface ExecResult {
  stdout: string;
  stderr: string;
  exitCode: number;
}

/** Result of running code. */
export interface Execution {
  /** The text output (stdout, trimmed). */
  text: string;
  stdout: string;
  stderr: string;
  exitCode: number;
  error?: string;
}

/** REPL helper script installed inside the VM for stateful code execution. */
const REPL_HELPER = `
import sys, ast, io

SEP = '# ---NOVAVM_BLOCK---'
source = open('/tmp/_novavm_code.py').read()
parts = source.split('\\n' + SEP + '\\n')
prev = '\\n'.join(parts[:-1]) if len(parts) > 1 else ''
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
`;

/**
 * A NovaVM sandbox running inside a KVM microVM.
 *
 * Connects to the nova-daemon REST API over HTTP.
 *
 * @example
 * ```typescript
 * import { Sandbox } from "novavm";
 *
 * const sandbox = await Sandbox.create({ image: "python:3.11-slim" });
 * await sandbox.runCode("x = 1");
 * const result = await sandbox.runCode("x += 1; x");
 * console.log(result.text); // "2"
 * await sandbox.destroy();
 * ```
 */
export class Sandbox {
  readonly sandboxId: string;
  readonly image: string;
  readonly vcpus: number;
  readonly memory: number;
  private readonly baseUrl: string;
  private readonly timeout: number;
  private started: boolean = false;
  private replInstalled: boolean = false;
  private codeBlocks: string[] = [];

  private constructor(opts: SandboxOptions = {}) {
    this.image = opts.image ?? "python:3.11-slim";
    this.vcpus = opts.vcpus ?? 1;
    this.memory = opts.memory ?? 256;
    this.sandboxId = opts.name ?? `novavm-${randomBytes(4).toString("hex")}`;
    this.baseUrl = (opts.baseUrl ?? "http://localhost:9800").replace(/\/+$/, "");
    this.timeout = opts.timeout ?? 30;
  }

  /**
   * Create and start a new sandbox.
   *
   * @example
   * ```typescript
   * const sb = await Sandbox.create({ image: "python:3.11-slim" });
   * ```
   */
  static async create(opts: SandboxOptions = {}): Promise<Sandbox> {
    const sb = new Sandbox(opts);
    await sb.start();
    return sb;
  }

  // ── HTTP helper ─────────────────────────────────────────

  private async request<T = any>(
    method: string,
    path: string,
    body?: Record<string, any>
  ): Promise<T> {
    const url = `${this.baseUrl}${path}`;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeout * 1000);

    try {
      const resp = await fetch(url, {
        method,
        headers: body ? { "Content-Type": "application/json" } : undefined,
        body: body ? JSON.stringify(body) : undefined,
        signal: controller.signal,
      });

      const text = await resp.text();
      const data = text ? JSON.parse(text) : {};

      if (!resp.ok) {
        throw new Error(
          `${method} ${path} failed (${resp.status}): ${data.error || text}`
        );
      }

      return data as T;
    } finally {
      clearTimeout(timer);
    }
  }

  // ── Lifecycle ──────────────────────────────────────────────

  /** Start the sandbox. Called automatically by `Sandbox.create()`. */
  async start(): Promise<void> {
    await this.request("POST", "/api/v1/sandboxes", {
      sandbox_id: this.sandboxId,
      image: this.image,
      vcpus: this.vcpus,
      memory: this.memory,
    });
    this.started = true;
  }

  /** Stop the sandbox gracefully. */
  async stop(): Promise<void> {
    if (this.started) {
      await this.request("POST", `/api/v1/sandboxes/${this.sandboxId}/stop`);
      this.started = false;
    }
  }

  /** Stop and remove the sandbox. */
  async destroy(): Promise<void> {
    try {
      await this.request("DELETE", `/api/v1/sandboxes/${this.sandboxId}`);
    } catch {
      // Already removed.
    }
    this.started = false;
  }

  // ── Command Execution ─────────────────────────────────────

  /** Execute a shell command inside the sandbox. */
  async exec(command: string, ...args: string[]): Promise<ExecResult> {
    const cmd = args.length > 0 ? `${command} ${args.join(" ")}` : command;
    try {
      const resp = await this.request<{
        stdout: string;
        stderr: string;
        exit_code: number;
      }>("POST", `/api/v1/sandboxes/${this.sandboxId}/exec`, {
        command: cmd,
      });
      return {
        stdout: resp.stdout ?? "",
        stderr: resp.stderr ?? "",
        exitCode: resp.exit_code ?? 0,
      };
    } catch (e: any) {
      return { stdout: "", stderr: String(e.message || e), exitCode: 1 };
    }
  }

  /**
   * Execute code with persistent state across calls.
   *
   * Bare expressions auto-print their value, like a Python REPL.
   *
   * @example
   * ```typescript
   * await sb.runCode("x = 1");
   * const result = await sb.runCode("x += 1; x");
   * console.log(result.text); // "2"
   * ```
   */
  async runCode(code: string): Promise<Execution> {
    if (!this.replInstalled) {
      await this.installReplHelper();
    }

    // Accumulate code blocks; the REPL helper re-executes all previous blocks
    // silently and only captures output from the latest block.
    this.codeBlocks.push(code);
    const fullCode = this.codeBlocks.join("\n# ---NOVAVM_BLOCK---\n");
    await this.exec(b64WriteCmd("/tmp/_novavm_code.py", fullCode));

    // Execute via the REPL helper.
    const result = await this.exec("python3 /tmp/_novavm_runner.py");

    // Strip kernel noise (e.g. "[   1.234] random: ..." lines from serial).
    const cleaned = result.stdout.replace(/\r\n/g, "\n").replace(/\r/g, "").replace(/\n$/, "");
    const lines = cleaned.split("\n");
    const cleanLines = lines.filter(
      (l) => !/^\[\s*\d+\.\d+\]/.test(l.trimStart())
    );
    const text = cleanLines.join("\n").trim();

    return {
      text,
      stdout: result.stdout,
      stderr: result.stderr,
      exitCode: result.exitCode,
      error: result.exitCode !== 0 ? result.stderr : undefined,
    };
  }

  // ── File Operations ───────────────────────────────────────

  /** Write a file inside the sandbox. */
  async writeFile(filePath: string, content: string): Promise<void> {
    await this.request(
      "POST",
      `/api/v1/sandboxes/${this.sandboxId}/files/write`,
      { path: filePath, content }
    );
  }

  /** Read a file from the sandbox. */
  async readFile(filePath: string): Promise<string> {
    const resp = await this.request<{ content: string }>(
      "POST",
      `/api/v1/sandboxes/${this.sandboxId}/files/read`,
      { path: filePath }
    );
    return resp.content ?? "";
  }

  // ── Internals ─────────────────────────────────────────────

  private async installReplHelper(): Promise<void> {
    await this.exec(b64WriteCmd("/tmp/_novavm_runner.py", REPL_HELPER));
    this.replInstalled = true;
  }
}
