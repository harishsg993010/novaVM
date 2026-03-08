"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.Sandbox = void 0;
const grpc = __importStar(require("@grpc/grpc-js"));
const protoLoader = __importStar(require("@grpc/proto-loader"));
const crypto_1 = require("crypto");
const path = __importStar(require("path"));
/** Encode content as base64 and return a single-line shell command to write it to a file. */
function b64WriteCmd(filePath, content) {
    const encoded = Buffer.from(content).toString("base64");
    return `echo ${encoded} | base64 -d > ${filePath}`;
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
// Load the proto definition once.
const PROTO_PATH = path.join(__dirname, "..", "proto", "runtime.proto");
const packageDefinition = protoLoader.loadSync(PROTO_PATH, {
    keepCase: false,
    longs: Number,
    enums: Number,
    defaults: true,
    oneofs: true,
});
const protoDescriptor = grpc.loadPackageDefinition(packageDefinition);
const novaRuntime = protoDescriptor.nova.runtime.v1;
/**
 * A NovaVM sandbox running inside a KVM microVM.
 *
 * Connects directly to the nova-daemon gRPC API over a Unix socket.
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
class Sandbox {
    constructor(opts = {}) {
        this.started = false;
        this.replInstalled = false;
        this.codeBlocks = [];
        this.image = opts.image ?? "python:3.11-slim";
        this.vcpus = opts.vcpus ?? 1;
        this.memory = opts.memory ?? 256;
        this.sandboxId = opts.name ?? `novavm-${(0, crypto_1.randomBytes)(4).toString("hex")}`;
        this.socket = opts.socket ?? "/var/run/nova/nova.sock";
        this.timeout = opts.timeout ?? 30;
        this.client = new novaRuntime.RuntimeService(`unix://${this.socket}`, grpc.credentials.createInsecure(), { "grpc.default_authority": "localhost" });
    }
    /**
     * Create and start a new sandbox.
     *
     * @example
     * ```typescript
     * const sb = await Sandbox.create({ image: "python:3.11-slim" });
     * ```
     */
    static async create(opts = {}) {
        const sb = new Sandbox(opts);
        await sb.start();
        return sb;
    }
    // ── gRPC helper ─────────────────────────────────────────
    rpc(method, request) {
        return new Promise((resolve, reject) => {
            const deadline = new Date(Date.now() + this.timeout * 1000);
            this.client[method](request, { deadline }, (err, response) => {
                if (err) {
                    reject(new Error(`${method} failed: ${err.details || err.message}`));
                }
                else {
                    resolve(response);
                }
            });
        });
    }
    // ── Lifecycle ──────────────────────────────────────────────
    /** Start the sandbox. Called automatically by `Sandbox.create()`. */
    async start() {
        // Step 1: Create the sandbox.
        await this.rpc("createSandbox", {
            sandboxId: this.sandboxId,
            image: this.image,
            config: {
                vcpus: this.vcpus,
                memoryMib: this.memory,
            },
        });
        // Step 2: Start the sandbox (boots the VM).
        await this.rpc("startSandbox", {
            sandboxId: this.sandboxId,
        });
        this.started = true;
    }
    /** Stop the sandbox gracefully. */
    async stop() {
        if (this.started) {
            await this.rpc("stopSandbox", { sandboxId: this.sandboxId });
            this.started = false;
        }
    }
    /** Stop and remove the sandbox. */
    async destroy() {
        try {
            await this.rpc("destroySandbox", { sandboxId: this.sandboxId });
        }
        catch {
            // Already removed.
        }
        this.started = false;
        this.client.close();
    }
    // ── Command Execution ─────────────────────────────────────
    /** Execute a shell command inside the sandbox. */
    async exec(command, ...args) {
        try {
            const resp = await this.rpc("execInSandbox", {
                sandboxId: this.sandboxId,
                command: [command, ...args],
            });
            const stdout = resp.stdout instanceof Buffer
                ? resp.stdout.toString("utf-8")
                : String(resp.stdout || "");
            const stderr = resp.stderr instanceof Buffer
                ? resp.stderr.toString("utf-8")
                : String(resp.stderr || "");
            return { stdout, stderr, exitCode: resp.exitCode ?? 0 };
        }
        catch (e) {
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
    async runCode(code) {
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
        const cleanLines = lines.filter((l) => !/^\[\s*\d+\.\d+\]/.test(l.trimStart()));
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
    async writeFile(filePath, content) {
        await this.exec(b64WriteCmd(filePath, content));
    }
    /** Read a file from the sandbox. */
    async readFile(filePath) {
        const result = await this.exec(`cat '${filePath}'`);
        return result.stdout;
    }
    // ── Internals ─────────────────────────────────────────────
    async installReplHelper() {
        await this.exec(b64WriteCmd("/tmp/_novavm_runner.py", REPL_HELPER));
        this.replInstalled = true;
    }
}
exports.Sandbox = Sandbox;
//# sourceMappingURL=sandbox.js.map