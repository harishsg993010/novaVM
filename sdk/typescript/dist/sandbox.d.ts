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
    /** Path to nova-daemon Unix socket. */
    socket?: string;
    /** gRPC call timeout in seconds (default: 30). */
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
export declare class Sandbox {
    readonly sandboxId: string;
    readonly image: string;
    readonly vcpus: number;
    readonly memory: number;
    private readonly socket;
    private readonly timeout;
    private started;
    private replInstalled;
    private codeBlocks;
    private client;
    private constructor();
    /**
     * Create and start a new sandbox.
     *
     * @example
     * ```typescript
     * const sb = await Sandbox.create({ image: "python:3.11-slim" });
     * ```
     */
    static create(opts?: SandboxOptions): Promise<Sandbox>;
    private rpc;
    /** Start the sandbox. Called automatically by `Sandbox.create()`. */
    start(): Promise<void>;
    /** Stop the sandbox gracefully. */
    stop(): Promise<void>;
    /** Stop and remove the sandbox. */
    destroy(): Promise<void>;
    /** Execute a shell command inside the sandbox. */
    exec(command: string, ...args: string[]): Promise<ExecResult>;
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
    runCode(code: string): Promise<Execution>;
    /** Write a file inside the sandbox. */
    writeFile(filePath: string, content: string): Promise<void>;
    /** Read a file from the sandbox. */
    readFile(filePath: string): Promise<string>;
    private installReplHelper;
}
//# sourceMappingURL=sandbox.d.ts.map